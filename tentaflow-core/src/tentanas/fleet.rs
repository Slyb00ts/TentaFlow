// =============================================================================
// File: tentanas/fleet.rs — the node list of the header (plan-02 §4). Every
//       node publishes a one-row summary of itself into the instance's
//       `addon_config` under `__nas_summary/<node_id>`; the rows travel with
//       the instance's config partition exactly like the platform's
//       `__node_status/<node_id>` rows, so the node that shows the dashboard
//       answers `NodesListRequest` from its own DB — no round trip to each
//       node, and an offline node still shows its last known state.
// =============================================================================

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tentaflow_protocol::tentanas::{NasDisk, NasNodeInfo};
use tentanas_helper::elastic::data_branch_path;

use crate::addon::native_apps::NODE_STATUS_KEY_PREFIX;
use crate::db::DbPool;
use crate::dispatch::HandlerContext;

pub const SUMMARY_KEY_PREFIX: &str = "__nas_summary/";

/// What one node says about itself.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeSummary {
    pub health: String,
    pub os_name: String,
    pub zfs_version: Option<String>,
    pub elevation_mode: String,
    pub disks_total: u32,
    pub disks_warning: u32,
    pub pools_total: u32,
    pub shares_total: u32,
    pub alerts_active: u32,
    pub capacity_bytes: u64,
    pub used_bytes: u64,
    pub updated_at: String,
    /// Short capability labels for the fleet "Funkcje" column. Defaulted so a
    /// summary written by an older build of this app still deserializes
    /// instead of dropping the whole row back to "unknown".
    #[serde(default)]
    pub features: Vec<String>,
    /// Installed RAM and the node's uptime at `updated_at`. Defaulted for the
    /// same reason as `features`.
    #[serde(default)]
    pub ram_bytes: u64,
    #[serde(default)]
    pub uptime_secs: u64,
    /// Disks whose health is 'critical', counted APART from `disks_warning`.
    /// The two used to be one number, so a node that had LOST a disk published
    /// it as a warning while `health` beside it already said 'critical' — and
    /// the chip that reads the count, not the grade, said "warning" too.
    /// `disks_warning` therefore keeps counting warnings only: the two are
    /// read side by side, never one minus the other.
    #[serde(default)]
    pub disks_critical: u32,
    /// Elastic Arrays, counted BESIDE `pools_total` rather than folded into
    /// it — a node whose only storage is an array reported zero pools, and
    /// the two kinds of storage are answered by different tools with
    /// different vocabularies.
    ///
    /// ALWAYS 0 in a PUBLISHED summary, like the array share of the two byte
    /// figures: an array belongs to one organisation and this row is read by
    /// every tenant of every node, so counting arrays here told each tenant
    /// how many arrays the others had and how big they were. The asking
    /// tenant's own arrays are added to THIS node's row at read time
    /// (`scope_local_arrays`). Kept as a field so an older build's row still
    /// deserializes.
    #[serde(default)]
    pub arrays_total: u32,
    /// Arrays left OUT of `capacity_bytes` and `used_bytes` because this node
    /// could not measure them. Non-zero means the two byte figures are
    /// PARTIAL, and the row says so instead of presenting a total that is
    /// quietly missing a disk shelf. Per tenant, like `arrays_total`.
    #[serde(default)]
    pub arrays_unmeasured: u32,
}

/// The capability labels of a node, from the same feature probe the
/// Environment tab shows — the fleet column is a summary of that table, not a
/// second opinion about what is installed. Only working features are listed:
/// a column of things the node cannot do is noise.
fn feature_labels(env: Option<&tentaflow_protocol::tentanas::NasEnvironment>) -> Vec<String> {
    let Some(env) = env else {
        return Vec::new();
    };
    env.features
        .iter()
        .filter(|f| f.status == "ok")
        .filter_map(|f| {
            let label = match f.id.as_str() {
                "zfs" => "OpenZFS",
                "samba" => "SMB",
                "nfs" => "NFS",
                "smartmontools" => "SMART",
                "nvme-cli" => "NVMe",
                "iscsi" => "iSCSI",
                "nvmet" => "NVMe-oF",
                "ledmon" => "LED",
                "mdadm" => "MD RAID",
                // A feature the probe grew without a label here is skipped
                // rather than shown by its internal id.
                _ => return None,
            };
            // The version is part of the label where it decides what the node
            // can do at all ("OpenZFS 2.3.1" — RAIDZ expansion or not).
            Some(match (f.id.as_str(), f.version.as_deref()) {
                ("zfs", Some(v)) => format!("{label} {v}"),
                _ => label.to_string(),
            })
        })
        .collect()
}

/// What a set of Elastic Arrays adds to the fleet row's two byte figures.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct ArrayBytes {
    capacity: u64,
    used: u64,
    /// Arrays counted in NEITHER figure because nothing measured them.
    unmeasured: u32,
}

/// The branch directories of one array's DATA disks — the same paths
/// `elastic::branch_to_protocol` files its measurements under, so the fleet
/// row and the array's own screen cannot disagree about which filesystem a
/// branch is.
fn data_branch_paths(array: &super::elastic::ElasticArrayRow) -> Vec<String> {
    array
        .data()
        .map(|b| data_branch_path(&array.name, &b.name))
        .collect()
}

/// Size and used bytes of every MOUNTPOINT one `df` printed.
///
/// Keyed by the mountpoint df reports (`--output=target`) and NOT by the path
/// it was handed, because that difference is the whole defence: a branch that
/// is not mounted makes df answer for the filesystem holding its empty
/// directory (`/`), so the branch's own path is simply absent from this map
/// and its array drops out of the totals instead of being credited with the
/// root filesystem's bytes.
fn parse_df(text: &str) -> BTreeMap<String, (u64, u64)> {
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [size, used, target] = fields.as_slice() else {
                return None;
            };
            Some((
                (*target).to_string(),
                (size.parse().ok()?, used.parse().ok()?),
            ))
        })
        .collect()
}

/// One `df` for every data branch of every array on the node.
///
/// ONE process for the whole node, not one per array: the row's single
/// `zpool list` is its whole budget and df takes as many paths as it is
/// given, so the node's arrays cost one more process, never one each. The
/// privileged `ElasticInspect` the Pools tab uses is the expensive answer to
/// the same question — a helper round trip, an elevation channel and a 30 s
/// timeout PER ARRAY — which a row published every minute must not pay.
async fn branch_usage(arrays: &[super::elastic::ElasticArrayRow]) -> BTreeMap<String, (u64, u64)> {
    let paths: Vec<String> = arrays.iter().flat_map(data_branch_paths).collect();
    if paths.is_empty() {
        return BTreeMap::new();
    }
    let mut args = vec!["-B1", "--output=size,used,target"];
    args.extend(paths.iter().map(String::as_str));
    // The exit code is not the verdict: df exits non-zero when ONE of the
    // paths is gone and still prints every row it did read, and a row it did
    // not print costs exactly its own array (`array_bytes`).
    match super::broker::run_unprivileged("df", &args, Duration::from_secs(10)).await {
        Ok(out) => parse_df(&out.stdout),
        Err(e) => {
            tracing::debug!("tentanas: df for array capacity failed: {e}");
            BTreeMap::new()
        }
    }
}

/// One array's (capacity, used), ALL OR NOTHING: `None` unless every data
/// branch was measured.
///
/// Capacity is the DATA branches and nothing else — parity adds no capacity
/// and the cache is a staging area — and it is summed PER BRANCH, never off
/// the union mount: MEASURED (2026-09-06, mergerfs 2.42.0) `df` on a union
/// reports ONE branch, so a union reading would understate a three-disk array
/// by two thirds and look plausible doing it (see `elastic::to_protocol`).
///
/// An array whose every data branch was measured goes into BOTH figures; one
/// with a single branch missing goes into neither. Capacity is not the safer
/// half of that pair to keep: capacity without usage understates how full the
/// node is, which is the defect this pairing exists to prevent.
fn one_array_bytes(
    array: &super::elastic::ElasticArrayRow,
    measured: &BTreeMap<String, (u64, u64)>,
) -> Option<(u64, u64)> {
    let branches = data_branch_paths(array);
    if branches.is_empty() {
        return None;
    }
    branches.iter().try_fold((0u64, 0u64), |(cap, used), path| {
        measured
            .get(path)
            .map(|(size, branch_used)| (cap + size, used + branch_used))
    })
}

/// Sums `readings` — one entry per array, `None` for an array nobody
/// measured — into the row's figures.
fn sum_arrays<'a>(readings: impl IntoIterator<Item = Option<&'a (u64, u64)>>) -> ArrayBytes {
    let mut out = ArrayBytes::default();
    for reading in readings {
        match reading {
            Some((cap, used)) => {
                out.capacity += cap;
                out.used += used;
            }
            None => out.unmeasured += 1,
        }
    }
    out
}

/// A set of arrays measured by one `df` — the per-array rule and the sum
/// together, which is what the measurement tests below hold to.
#[cfg(test)]
fn array_bytes(
    arrays: &[super::elastic::ElasticArrayRow],
    measured: &BTreeMap<String, (u64, u64)>,
) -> ArrayBytes {
    let readings: Vec<Option<(u64, u64)>> =
        arrays.iter().map(|a| one_array_bytes(a, measured)).collect();
    sum_arrays(readings.iter().map(Option::as_ref))
}

/// Every array of THIS node by name, with its (capacity, used) from the last
/// published summary's `df`, or `None` when that pass could not measure it.
///
/// Held in the process rather than published: it is per array, so per
/// organisation, and the published row is read by every tenant. The read
/// path (`scope_local_arrays`) picks the asking tenant's arrays out of it,
/// which costs no `df` per request — the figures are exactly as fresh as the
/// rest of the row they are added to. Names key it because an array's name is
/// unique on a node (its union is `/mnt/<name>`).
fn local_array_readings() -> &'static parking_lot::RwLock<BTreeMap<String, Option<(u64, u64)>>> {
    static READINGS: std::sync::OnceLock<parking_lot::RwLock<BTreeMap<String, Option<(u64, u64)>>>> =
        std::sync::OnceLock::new();
    READINGS.get_or_init(Default::default)
}

/// Warnings and failures of the node's disks, counted APART. `health` below
/// already tells the two states apart; one shared counter beside it let a
/// dead disk travel to the fleet row as a warning.
fn disk_counts(disks: &[NasDisk]) -> (u32, u32) {
    let count = |grade: &str| disks.iter().filter(|d| d.health == grade).count() as u32;
    (count("warning"), count("critical"))
}

pub async fn local_summary(db: &DbPool) -> NodeSummary {
    let (disks, _) = super::disks::snapshot();
    let env = super::environment::cached_or_probe(db).await.ok();
    let (disks_warning, disks_critical) = disk_counts(&disks);
    // Elastic Arrays are storage this node serves, but each belongs to ONE
    // organisation, so they are measured here and NOT published: which
    // filesystems they are comes from the node's own rows, how full they are
    // from one `df` over those branches, and the result waits in the process
    // for `scope_local_arrays` to add the asking tenant's own to its row.
    let arrays = super::db::elastic_arrays_all(db).unwrap_or_default();
    let measured = branch_usage(&arrays).await;
    *local_array_readings().write() = arrays
        .iter()
        .map(|a| (a.name.clone(), one_array_bytes(a, &measured)))
        .collect();
    // One `zpool list` is enough for the fleet row: the full pool view costs a
    // `zpool status` per pool and the header only needs counts and states.
    let pools = super::pools::list_rows().await.unwrap_or_default();
    let pool_critical = pools
        .iter()
        .any(|p| matches!(p.state.as_str(), "faulted" | "unavail" | "removed"));
    let pool_warning = pools
        .iter()
        .any(|p| matches!(p.state.as_str(), "degraded" | "offline") || p.capacity_pct >= 80);
    // Shares are NOT counted here (migration 20): each belongs to one
    // organisation and this row is read by every tenant of every node, so a
    // count — or a "warning" caused by one tenant's broken share — would tell
    // the others about it. The asking tenant's own shares, and the warning a
    // share of theirs that the node cannot export deserves, are added to
    // THIS node's row at read time (`scope_local_shares`).
    let health = if disks_critical > 0 || pool_critical {
        "critical"
    } else if disks_warning > 0 || pool_warning {
        "warning"
    } else {
        "ok"
    };
    NodeSummary {
        health: health.to_string(),
        os_name: env.as_ref().map(|e| e.os_name.clone()).unwrap_or_default(),
        zfs_version: env
            .as_ref()
            .and_then(|e| e.features.iter().find(|f| f.id == "zfs"))
            .and_then(|f| f.version.clone()),
        elevation_mode: super::elevation::mode(db).as_str().to_string(),
        disks_total: disks.len() as u32,
        disks_warning,
        pools_total: pools.len() as u32,
        // Per tenant, so never published (see above).
        shares_total: 0,
        // NODE-WIDE alerts only: this row is published into the instance's
        // `addon_config`, which every tenant of every node reads, so it may
        // not count (and thereby reveal) any tenant's own alerts. The asking
        // tenant's figure for THIS node is completed at read time
        // (`scope_local_alerts`).
        alerts_active: super::db::count_open_node_alerts(db).unwrap_or(0),
        // BOTH figures come from the same set, which is the point of them.
        // Capacity used to sum the node's RAW DISKS while used summed the ZFS
        // pools, so the percentage compared a pool's allocation against disks
        // that were never in a pool — every free disk, every system disk and
        // every parity disk made the node look emptier than it was. What is
        // summed is the storage the node actually serves: here its ZFS pools
        // (node hardware, every tenant's to see), and at read time the asking
        // tenant's own measured Elastic Arrays (`scope_local_arrays`).
        capacity_bytes: pools.iter().map(|p| p.size_bytes).sum::<u64>(),
        used_bytes: pools.iter().map(|p| p.alloc_bytes).sum::<u64>(),
        features: feature_labels(env.as_ref()),
        // Host facts of the node card's subtitle come from the environment
        // probe, the one source the Environment tab already reads them from.
        // The uptime is a reading, not a clock: it is what the node had at
        // `updated_at`, and the card shows it in days.
        ram_bytes: env.as_ref().map(|e| e.ram_bytes).unwrap_or(0),
        uptime_secs: env.as_ref().map(|e| e.uptime_secs).unwrap_or(0),
        disks_critical,
        // Per tenant, so never published (see the field).
        arrays_total: 0,
        arrays_unmeasured: 0,
        updated_at: super::db::now(),
    }
}

/// Writes this node's summary row. Best-effort like `record_node_status`.
pub async fn publish_local_summary(main_db: &DbPool, addon_id: &str, db: &DbPool) {
    let summary = local_summary(db).await;
    let node_id = crate::sync::runtime::local_node_id().unwrap_or_else(|| "local".to_string());
    let Ok(value) = serde_json::to_string(&summary) else { return };
    if let Err(e) = crate::db::repository::upsert_addon_config_value(
        main_db,
        addon_id,
        &format!("{SUMMARY_KEY_PREFIX}{node_id}"),
        &value,
        false,
        None,
    ) {
        tracing::warn!("tentanas: summary publish failed: {e}");
    }
}

/// Replaces the local node's published alert figure with the count the
/// asking organisation's alert list shows: node-wide alerts plus its own
/// (`db::count_open_alerts_for_org`, the list's own visibility clause).
///
/// Only the LOCAL row can be completed: this node has its own database and
/// nothing else. A remote node's row keeps the node-wide figure it published,
/// which may undercount the tenant's own alerts there but never counts
/// another tenant's. A failed read keeps the node-wide figure for the same
/// reason.
pub fn scope_local_alerts<'v>(nodes: &mut [NasNodeInfo], db: &DbPool, viewer: impl Into<super::db::OrgViewer<'v>>) {
    let Ok(count) = super::db::count_open_alerts_for_org(db, viewer) else {
        return;
    };
    for node in nodes.iter_mut().filter(|n| n.is_local) {
        node.alerts_active = count;
    }
}

/// Adds the asking organisation's own Elastic Arrays to THIS node's row:
/// their count, and their measured bytes on top of the pools' — the array
/// half of the figures the published summary leaves out.
///
/// The same shape as `scope_local_alerts`, for the same reason: the published
/// row is read by every tenant of every node, so it carries node-wide figures
/// only (pools), and only the local row can be completed, because only this
/// node's database knows whose arrays are whose. A remote row keeps its
/// pools-only figures — it undercounts the tenant's arrays there but never
/// reveals another tenant's. A failed read adds nothing, for the same reason.
///
/// Returns whether the tenant's arrays were counted: false for a caller with
/// no organisation and for a failed read (`scope_local_org_figures`).
pub fn scope_local_arrays(nodes: &mut [NasNodeInfo], db: &DbPool, org_id: &str) -> bool {
    if org_id.is_empty() {
        return false;
    }
    let Ok(own) = super::db::elastic_array_names_of_org(db, org_id) else {
        return false;
    };
    add_own_arrays(nodes, &own, &local_array_readings().read());
    true
}

/// Puts the asking organisation's own shares on THIS node's row: their count,
/// and "warning" when one of them is in `error` (a share the node cannot
/// export is a node problem the fleet view has to show — for the tenant it
/// belongs to). The same shape and the same limits as `scope_local_alerts`:
/// the published row carries no share figure at all, only the local row can
/// be completed, and a remote row keeps 0 — it undercounts the tenant's
/// shares there but never reveals another tenant's. A failed read leaves the
/// row as published.
///
/// Returns whether the read succeeded (`scope_local_org_figures`).
pub fn scope_local_shares(nodes: &mut [NasNodeInfo], db: &DbPool, org_id: &str) -> bool {
    let Ok((total, errors)) = super::db::share_counts(db, org_id) else {
        return false;
    };
    add_own_shares(nodes, total, errors);
    true
}

/// Both per-organisation completions of THIS node's row — its arrays and its
/// shares — and the flag that says they happened (`per_org_counted`).
///
/// WHY the flag: a scoped read that fails leaves the published figure, which
/// is 0 for shares and arrays alike, and a 0 on the local row read exactly
/// like "this tenant has none here". The front used `is_local` as "counted",
/// so a database hiccup told a tenant its shares were gone. The flag is set
/// only when BOTH reads succeeded for a caller with an organisation;
/// anything else reads "not counted", like a remote row.
pub fn scope_local_org_figures(nodes: &mut [NasNodeInfo], db: &DbPool, org_id: &str) {
    let arrays = scope_local_arrays(nodes, db, org_id);
    let shares = scope_local_shares(nodes, db, org_id);
    // `scope_local_arrays` already answers false for no organisation.
    mark_local_counted(nodes, arrays && shares);
}

fn mark_local_counted(nodes: &mut [NasNodeInfo], counted: bool) {
    for node in nodes.iter_mut().filter(|n| n.is_local) {
        node.per_org_counted = counted;
    }
}

fn add_own_shares(nodes: &mut [NasNodeInfo], total: u32, errors: u32) {
    for node in nodes.iter_mut().filter(|n| n.is_local) {
        node.shares_total = total;
        if errors > 0 && node.health == "ok" {
            node.health = "warning".to_string();
        }
    }
}

/// `scope_local_arrays` over what it read. An own array missing from the
/// readings (created after the last summary) is unmeasured, never zero bytes.
fn add_own_arrays(
    nodes: &mut [NasNodeInfo],
    own: &std::collections::BTreeSet<String>,
    readings: &BTreeMap<String, Option<(u64, u64)>>,
) {
    let sums = sum_arrays(own.iter().map(|name| readings.get(name).and_then(Option::as_ref)));
    for node in nodes.iter_mut().filter(|n| n.is_local) {
        node.arrays_total = own.len() as u32;
        node.arrays_unmeasured = sums.unmeasured;
        node.capacity_bytes += sums.capacity;
        node.used_bytes += sums.used;
    }
}

fn prefixed_map(db: &DbPool, addon_id: &str, prefix: &str) -> HashMap<String, String> {
    crate::db::repository::list_addon_config_prefixed(db, addon_id, prefix)
        .unwrap_or_default()
        .into_iter()
        .map(|(node, value, _)| (node, value))
        .collect()
}

/// THE PEER STORE NEVER HOLDS THIS NODE. `seed_discovered_peer` returns early
/// on `node_id == local_node_id`, so asking it for the local hostname always
/// answers `None` and the fallback used to publish the 64-hex node id as the
/// node's NAME — on every install, in every fleet view. The machine's own
/// hostname is what the rest of the core already shows for itself, so it is
/// what this asks first for the local row.
///
/// A peer the store has no hostname for (paired, offline since this node
/// started) falls back to the name the sync registry recorded for it, and
/// then to NOTHING. An empty name is deliberate: the 64-hex node id used to
/// fill that gap and was printed as the node's name on every fleet surface,
/// while the screen has one helper (`nodeLabel`) that says "Węzeł bez nazwy"
/// and keeps the id in a tooltip.
pub(crate) fn node_name(ctx: &HandlerContext, node_id: &str) -> String {
    if node_id == ctx.state.local_node_id.to_string() {
        let local = crate::mesh::node_info_collector::local_hostname();
        if !local.is_empty() && local != "unknown" {
            return local;
        }
    }
    ctx.state
        .mesh_peer_store
        .get_hostname(node_id)
        .filter(|n| !n.is_empty())
        .or_else(|| registry_name(ctx, node_id))
        .unwrap_or_default()
}

/// `sync_nodes.display_name` of one node, when the registry has a non-empty
/// one. Asked only after the peer store came up empty, so a healthy fleet
/// never pays for it.
fn registry_name(ctx: &HandlerContext, node_id: &str) -> Option<String> {
    crate::db::repository::lookup_sync_node_info(&ctx.state.db, &[node_id.to_string()])
        .ok()?
        .remove(node_id)
        .map(|(name, _)| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// This node plus every trust-paired peer, each with its instance status
/// and its last published summary.
pub fn nodes(ctx: &HandlerContext, addon_id: &str) -> Vec<NasNodeInfo> {
    let statuses = prefixed_map(&ctx.state.db, addon_id, NODE_STATUS_KEY_PREFIX);
    let summaries = prefixed_map(&ctx.state.db, addon_id, SUMMARY_KEY_PREFIX);
    let local_id = ctx.state.local_node_id.to_string();

    let mut ids: Vec<(String, bool)> = vec![(local_id.clone(), true)];
    if let Some(iroh) = ctx.state.quic_mesh.as_ref() {
        for peer in ctx.state.mesh_peer_store.list() {
            if peer.node_id == local_id || !iroh.is_trusted(&peer.node_id) {
                continue;
            }
            ids.push((peer.node_id.clone(), peer.quic_connected));
        }
    }

    ids.into_iter()
        .map(|(node_id, online)| {
            let instance_status = statuses
                .get(&node_id)
                .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok())
                .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string());
            let s = summaries
                .get(&node_id)
                .and_then(|v| serde_json::from_str::<NodeSummary>(v).ok())
                .unwrap_or_default();
            NasNodeInfo {
                node_name: node_name(ctx, &node_id),
                is_local: node_id == local_id,
                online,
                instance_status,
                health: if s.updated_at.is_empty() { "unknown".to_string() } else { s.health },
                os_name: s.os_name,
                zfs_version: s.zfs_version,
                elevation_mode: s.elevation_mode,
                disks_total: s.disks_total,
                disks_warning: s.disks_warning,
                pools_total: s.pools_total,
                shares_total: s.shares_total,
                alerts_active: s.alerts_active,
                capacity_bytes: s.capacity_bytes,
                used_bytes: s.used_bytes,
                features: s.features,
                ram_bytes: s.ram_bytes,
                uptime_secs: s.uptime_secs,
                disks_critical: s.disks_critical,
                arrays_total: s.arrays_total,
                arrays_unmeasured: s.arrays_unmeasured,
                // Set by `scope_local_org_figures`, on this node's row only.
                per_org_counted: false,
                updated_at: (!s.updated_at.is_empty()).then_some(s.updated_at),
                node_id,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::elastic::{BranchRow, ElasticArrayRow};

    /// `df -B1 --output=size,used,target` of a three-branch array, LC_ALL=C as
    /// the broker runs it. The fourth path was handed in too and is NOT here
    /// under its own name: that branch is unmounted, so df answered for the
    /// root filesystem that holds its empty directory.
    const DF: &str = "   1B-blocks         Used Mounted on\n\
4000769048576 1000000000000 /mnt/tentanas-branches/media/data/d1\n\
4000769048576 2000000000000 /mnt/tentanas-branches/media/data/d2\n\
4000769048576  500000000000 /mnt/tentanas-branches/media/data/d3\n\
 997568933888 545454268416 /\n";

    fn array(name: &str, data: usize, cache: bool) -> ElasticArrayRow {
        let mut branches: Vec<BranchRow> = (1..=data)
            .map(|i| BranchRow {
                name: format!("d{i}"),
                role: "data".to_string(),
                ..Default::default()
            })
            .collect();
        if cache {
            branches.push(BranchRow {
                name: "c1".to_string(),
                role: "cache".to_string(),
                ..Default::default()
            });
        }
        ElasticArrayRow { name: name.to_string(), branches, ..Default::default() }
    }

    fn disk(health: &str) -> NasDisk {
        NasDisk { health: health.to_string(), ..Default::default() }
    }

    /// The header line is not a row, and a branch is found by ITS OWN path or
    /// not at all — the unmounted branch must not inherit the root
    /// filesystem's 997 GB just because df answered for `/` in its place.
    #[test]
    fn df_rows_are_keyed_by_the_mountpoint_df_answered_for() {
        let rows = parse_df(DF);
        assert_eq!(rows.len(), 4, "header parsed as a row: {rows:?}");
        assert_eq!(
            rows.get("/mnt/tentanas-branches/media/data/d2"),
            Some(&(4_000_769_048_576, 2_000_000_000_000))
        );
        assert!(rows.contains_key("/"));
        assert!(!rows.contains_key("/mnt/tentanas-branches/media/data/d4"));
    }

    /// The measured trap: one `df` on the union reports ONE branch, so an
    /// array of three 4 TB disks must come out at 12 TB here, never at 4.
    #[test]
    fn an_array_is_the_sum_of_its_data_branches() {
        let bytes = array_bytes(&[array("media", 3, false)], &parse_df(DF));
        assert_eq!(bytes.capacity, 3 * 4_000_769_048_576);
        assert_eq!(bytes.used, 3_500_000_000_000);
        assert_eq!(bytes.unmeasured, 0);
    }

    /// A branch nobody measured costs its array BOTH figures. Keeping the
    /// capacity of the branches that did answer would report a fuller array
    /// as an emptier one, which is the defect the pairing exists to prevent.
    #[test]
    fn an_array_with_one_unmeasured_branch_is_counted_in_neither_figure() {
        let bytes = array_bytes(&[array("media", 4, false)], &parse_df(DF));
        assert_eq!(bytes, ArrayBytes { capacity: 0, used: 0, unmeasured: 1 });
    }

    /// An array with no data branch recorded at all is unmeasured, not an
    /// array of size zero — zero would read as "measured and empty".
    #[test]
    fn an_array_without_data_branches_is_unmeasured() {
        let bytes = array_bytes(&[array("empty", 0, true)], &BTreeMap::new());
        assert_eq!(bytes.unmeasured, 1);
        assert_eq!(bytes.capacity, 0);
    }

    /// The cache is a staging area, not capacity an admin can plan with, and
    /// it is not asked for: an array whose cache is unmounted still reports
    /// its data branches in full.
    #[test]
    fn a_cache_branch_is_neither_capacity_nor_a_reason_to_drop_the_array() {
        let mut df = parse_df(DF);
        df.insert("/mnt/tentanas-branches/media/cache/c1".to_string(), (500, 400));
        let bytes = array_bytes(&[array("media", 3, true)], &df);
        assert_eq!(bytes.capacity, 3 * 4_000_769_048_576, "cache in the total");
        assert_eq!(bytes.unmeasured, 0);
    }

    /// Two arrays, one measured and one not: the measured one still counts.
    /// A single blind array must not blank the node's whole capacity.
    #[test]
    fn one_unmeasured_array_does_not_take_the_others_down_with_it() {
        let bytes = array_bytes(&[array("media", 3, false), array("backup", 2, false)], &parse_df(DF));
        assert_eq!(bytes.capacity, 3 * 4_000_769_048_576);
        assert_eq!(bytes.unmeasured, 1);
    }

    /// A dead disk is a failure. It used to be counted into the warning
    /// total, so the fleet row said "warning" about a node that had lost a
    /// disk, while `health` on the same row said 'critical'.
    #[test]
    fn a_dead_disk_is_a_failure_and_not_a_warning() {
        let disks = [
            disk("ok"),
            disk("warning"),
            disk("critical"),
            disk("critical"),
            disk("unknown"),
        ];
        assert_eq!(disk_counts(&disks), (1, 2));
        assert_eq!(disk_counts(&[disk("critical")]), (0, 1), "failure counted as a warning");
        assert_eq!(disk_counts(&[]), (0, 0));
    }

    /// A summary an older build wrote carries neither counter; it must still
    /// deserialize, and it must not invent failures or arrays.
    #[test]
    fn a_summary_without_the_new_counters_still_deserializes() {
        let json = r#"{"health":"warning","os_name":"CachyOS","zfs_version":null,
            "elevation_mode":"helper","disks_total":4,"disks_warning":1,"pools_total":1,
            "shares_total":2,"alerts_active":0,"capacity_bytes":10,"used_bytes":5,
            "updated_at":"2026-09-15 10:00:00"}"#;
        let s: NodeSummary = serde_json::from_str(json).expect("older summary decodes");
        assert_eq!(s.disks_warning, 1);
        assert_eq!(s.disks_critical, 0);
        assert_eq!(s.arrays_total, 0);
        assert_eq!(s.arrays_unmeasured, 0);
    }

    /// The published row carries pools only; THIS node's row gets the asking
    /// tenant's own arrays added — their count, their measured bytes, and
    /// how many of them nobody measured — and never another tenant's. A
    /// remote row is left exactly as it was published.
    #[test]
    fn the_local_row_adds_only_the_asking_tenants_arrays() {
        let readings: BTreeMap<String, Option<(u64, u64)>> = [
            ("alpha".to_string(), Some((100, 40))),
            ("alpha-cold".to_string(), None),
            ("bravo".to_string(), Some((9_000, 8_000))),
        ]
        .into_iter()
        .collect();
        let fleet = || {
            vec![
                NasNodeInfo {
                    node_id: "local".into(),
                    is_local: true,
                    capacity_bytes: 1_000,
                    used_bytes: 500,
                    ..Default::default()
                },
                NasNodeInfo {
                    node_id: "peer".into(),
                    capacity_bytes: 7,
                    used_bytes: 3,
                    ..Default::default()
                },
            ]
        };
        let set = |names: &[&str]| -> std::collections::BTreeSet<String> {
            names.iter().map(|n| n.to_string()).collect()
        };
        // `alpha-new` was created after the last summary: unmeasured, not 0 B.
        for (org, own, expected) in [
            ("org-a", set(&["alpha", "alpha-cold", "alpha-new"]), (3, 2, 1_100, 540)),
            ("org-b", set(&["bravo"]), (1, 0, 10_000, 8_500)),
            ("org-c", set(&[]), (0, 0, 1_000, 500)),
        ] {
            let mut nodes = fleet();
            add_own_arrays(&mut nodes, &own, &readings);
            let local = &nodes[0];
            assert_eq!(
                (local.arrays_total, local.arrays_unmeasured, local.capacity_bytes, local.used_bytes),
                expected,
                "{org}"
            );
            let peer = &nodes[1];
            assert_eq!(
                (peer.arrays_total, peer.arrays_unmeasured, peer.capacity_bytes, peer.used_bytes),
                (0, 0, 7, 3),
                "{org}: a remote row is not this node's to complete"
            );
        }
    }

    /// Which arrays are the tenant's comes from this node's own rows, by
    /// organisation; a caller with no organisation gets nothing added.
    #[test]
    fn the_local_row_counts_the_arrays_the_asking_org_owns_on_this_node() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        super::super::db::migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO nas_elastic_arrays
               (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at) VALUES
               ('arr-a','org-a','nas','fleet-scope-alpha','xfs','active','','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z'),
               ('arr-b','org-b','nas','fleet-scope-bravo','xfs','active','','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z'),
               ('arr-c','org-b','nas','fleet-scope-charlie','xfs','active','','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z');",
        )
        .unwrap();
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let local = || vec![NasNodeInfo { node_id: "local".into(), is_local: true, ..Default::default() }];
        for (org, total) in [("org-a", 1), ("org-b", 2), ("org-c", 0), ("", 0)] {
            let mut nodes = local();
            scope_local_arrays(&mut nodes, &db, org);
            assert_eq!(nodes[0].arrays_total, total, "{org:?}");
        }
    }

    /// The fleet badge of THIS node, per tenant: node-wide alerts plus the
    /// asking organisation's own — never another tenant's — while a remote
    /// row keeps the node-wide figure it published.
    #[test]
    fn the_local_alert_badge_counts_what_the_asking_tenant_is_shown() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        super::super::db::migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO nas_elastic_arrays
               (array_id,org_id,addon_id,name,filesystem,state,state_detail,created_at,updated_at) VALUES
               ('arr-a','org-a','nas','alpha','xfs','active','','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z'),
               ('arr-b','org-b','nas','bravo','xfs','active','','2026-09-01T00:00:00Z','2026-09-01T00:00:00Z');",
        )
        .unwrap();
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        use super::super::db::raise_alert;
        raise_alert(&db, "elastic:alpha:x", "warning", "elastic-array", "alpha", "Macierz alpha", "").unwrap();
        raise_alert(&db, "elastic:bravo:x", "warning", "elastic-array", "bravo", "Macierz bravo", "").unwrap();
        raise_alert(&db, "elastic:bravo:y", "warning", "elastic-array", "bravo", "Macierz bravo 2", "").unwrap();
        raise_alert(&db, "disk:d1:temp", "warning", "disk", "d1", "Disk sda: hot", "").unwrap();
        // What `local_summary` publishes for every tenant to read.
        assert_eq!(super::super::db::count_open_node_alerts(&db).unwrap(), 1);

        let fleet = || vec![
            NasNodeInfo { node_id: "local".into(), is_local: true, alerts_active: 1, ..Default::default() },
            NasNodeInfo { node_id: "peer".into(), is_local: false, alerts_active: 4, ..Default::default() },
        ];
        for (org, local) in [("org-a", 2), ("org-b", 3), ("org-c", 1)] {
            let mut nodes = fleet();
            scope_local_alerts(&mut nodes, &db, org);
            assert_eq!((nodes[0].alerts_active, nodes[1].alerts_active), (local, 4), "{org}");
            assert_eq!(
                super::super::db::list_alerts_for_org(&db, org, false).unwrap().len() as u32,
                local,
                "{org}: the badge and the list agree"
            );
        }
    }

    /// The published row carries no share figure (every tenant reads it);
    /// the local row gets the asking tenant's own count and turns "warning"
    /// only for a broken share of THAT tenant — never downgrading a worse
    /// grade, and never touching a remote row.
    #[test]
    fn the_local_row_counts_only_the_callers_shares() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        super::super::db::migrate(&conn).unwrap();
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let share = |id: &str, name: &str| super::super::db::ShareRow {
            share_id: id.into(),
            name: name.into(),
            protocol: "nfs".into(),
            source_path: format!("/mnt/tank/{name}"),
            nfs: Some(Default::default()),
            state: "active".into(),
            ..Default::default()
        };
        super::super::db::upsert_share(&db, "org-a", &share("a1", "projekty")).unwrap();
        super::super::db::upsert_share(&db, "org-b", &share("b1", "kadry")).unwrap();
        super::super::db::upsert_share(&db, "org-b", &share("b2", "place")).unwrap();
        super::super::db::set_share_state(&db, "b2", "error", "source path is not mounted").unwrap();

        let rows = || {
            vec![
                NasNodeInfo { is_local: true, health: "ok".into(), shares_total: 9, ..Default::default() },
                NasNodeInfo { is_local: false, health: "ok".into(), shares_total: 0, ..Default::default() },
            ]
        };
        let mut a = rows();
        scope_local_shares(&mut a, &db, "org-a");
        assert_eq!((a[0].shares_total, a[0].health.as_str()), (1, "ok"), "org B's broken share is not A's warning");
        assert_eq!(a[1].shares_total, 0);
        let mut b = rows();
        scope_local_shares(&mut b, &db, "org-b");
        assert_eq!((b[0].shares_total, b[0].health.as_str()), (2, "warning"));
        assert_eq!(b[1].health, "ok", "a remote row is never touched");
        let mut critical = rows();
        critical[0].health = "critical".into();
        scope_local_shares(&mut critical, &db, "org-b");
        assert_eq!(critical[0].health, "critical");
    }

    /// `per_org_counted` is what lets the front tell a real 0 from a figure
    /// nobody counted: true on THIS node's row only when both scoped reads
    /// succeeded for a caller with an organisation — never on a remote row,
    /// never for a caller without an organisation, never after a failed read.
    #[test]
    fn the_local_row_is_counted_only_when_both_scoped_reads_succeed() {
        let migrated = || {
            let conn = rusqlite::Connection::open_in_memory().unwrap();
            super::super::db::migrate(&conn).unwrap();
            conn
        };
        let pool = |conn: rusqlite::Connection| -> DbPool { std::sync::Arc::new(crate::db::Db::from_connection(conn)) };
        let rows = || {
            vec![
                NasNodeInfo { node_id: "local".into(), is_local: true, ..Default::default() },
                NasNodeInfo { node_id: "peer".into(), is_local: false, ..Default::default() },
            ]
        };
        let counted = |db: &DbPool, org: &str| {
            let mut nodes = rows();
            scope_local_org_figures(&mut nodes, db, org);
            (nodes[0].per_org_counted, nodes[1].per_org_counted)
        };

        // Both reads succeed — an organisation with nothing on this node is a
        // counted 0, which is exactly the case the flag exists for.
        let db = pool(migrated());
        assert_eq!(counted(&db, "org-a"), (true, false));
        // No organisation: the arrays are not read at all.
        assert_eq!(counted(&db, ""), (false, false));

        // The share read fails, the array read does not.
        let conn = migrated();
        conn.execute_batch("DROP TABLE nas_shares;").unwrap();
        assert_eq!(counted(&pool(conn), "org-a"), (false, false), "a failed share read is not a counted 0");

        // The array read fails, the share read does not.
        let conn = migrated();
        conn.execute_batch("DROP TABLE nas_elastic_arrays;").unwrap();
        assert_eq!(counted(&pool(conn), "org-a"), (false, false), "a failed array read is not a counted 0");

        // A row as `nodes` builds it, before any scoping, is not counted.
        assert!(rows().iter().all(|n| !n.per_org_counted));
    }
}
