// =============================================================================
// File: tentanas/fleet.rs — the node list of the header (plan-02 §4). Every
//       node publishes a one-row summary of itself into the instance's
//       `addon_config` under `__nas_summary/<node_id>`; the rows travel with
//       the instance's config partition exactly like the platform's
//       `__node_status/<node_id>` rows, so the node that shows the dashboard
//       answers `NodesListRequest` from its own DB — no round trip to each
//       node, and an offline node still shows its last known state.
// =============================================================================

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tentaflow_protocol::tentanas::NasNodeInfo;

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

pub async fn local_summary(db: &DbPool) -> NodeSummary {
    let (disks, _) = super::disks::snapshot();
    let env = super::environment::cached_or_probe(db).await.ok();
    let disks_warning = disks
        .iter()
        .filter(|d| matches!(d.health.as_str(), "warning" | "critical"))
        .count() as u32;
    // One `zpool list` is enough for the fleet row: the full pool view costs a
    // `zpool status` per pool and the header only needs counts and states.
    let pools = super::pools::list_rows().await.unwrap_or_default();
    let pool_critical = pools
        .iter()
        .any(|p| matches!(p.state.as_str(), "faulted" | "unavail" | "removed"));
    let pool_warning = pools
        .iter()
        .any(|p| matches!(p.state.as_str(), "degraded" | "offline") || p.capacity_pct >= 80);
    // A share the node cannot export (unmounted source, missing service) is a
    // node problem the fleet view has to show, not a detail of one tab.
    let (shares_total, shares_error) = super::db::share_counts(db).unwrap_or((0, 0));
    let health = if disks.iter().any(|d| d.health == "critical") || pool_critical {
        "critical"
    } else if disks_warning > 0 || pool_warning || shares_error > 0 {
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
        shares_total,
        alerts_active: super::db::count_open_alerts(db).unwrap_or(0),
        capacity_bytes: disks.iter().map(|d| d.size_bytes).sum(),
        // Raw allocated bytes of the pools: what the fleet row compares against
        // the node's raw disk capacity above.
        used_bytes: pools.iter().map(|p| p.alloc_bytes).sum(),
        features: feature_labels(env.as_ref()),
        // Host facts of the node card's subtitle come from the environment
        // probe, the one source the Environment tab already reads them from.
        // The uptime is a reading, not a clock: it is what the node had at
        // `updated_at`, and the card shows it in days.
        ram_bytes: env.as_ref().map(|e| e.ram_bytes).unwrap_or(0),
        uptime_secs: env.as_ref().map(|e| e.uptime_secs).unwrap_or(0),
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

fn prefixed_map(db: &DbPool, addon_id: &str, prefix: &str) -> HashMap<String, String> {
    crate::db::repository::list_addon_config_prefixed(db, addon_id, prefix)
        .unwrap_or_default()
        .into_iter()
        .map(|(node, value, _)| (node, value))
        .collect()
}

fn node_name(ctx: &HandlerContext, node_id: &str) -> String {
    ctx.state
        .mesh_peer_store
        .get_hostname(node_id)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| node_id.to_string())
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
                updated_at: (!s.updated_at.is_empty()).then_some(s.updated_at),
                node_id,
            }
        })
        .collect()
}
