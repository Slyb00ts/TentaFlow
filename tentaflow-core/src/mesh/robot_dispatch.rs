// =============================================================================
// File: mesh/robot_dispatch.rs
// Purpose: SENDER + DISCOVERY side of cross-node robot control. Exactly ONE node
//          owns a robot addon + physical access; every other node controls it by
//          sending a `MeshCommandType::RobotControl` to the owner. This module:
//            - advertises which robots THIS node owns (a compact in-memory robot
//              registry, fed from the local enabled `[robot] controls_robot=true`
//              addons), and ingests peers' advertisements over the mesh;
//            - resolves the owner of a `robot_id` (Local / Remote(node) / Unknown)
//              via a PURE, unit-testable selection over advertised entries;
//            - builds a fresh `RobotControlRequest` (uuid, expiry window, sender-
//              side velocity clamp) and routes it: Local → the SAME shared local-
//              execute path the receiver uses; Remote → CBOR over the mesh.
//          The mesh send is behind a small `RobotCommandSender` trait so the
//          routing decision (local/remote/unknown + request building) is testable
//          with a fake sender — no mesh, no robot, no second node required.
// =============================================================================

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::db::DbPool;
use crate::mesh::robot_control::{
    plan_execution, RobotAction, RobotControlRequest, RobotControlResponse, RejectReason,
    MAX_MOVE_DURATION_MS, MAX_VELOCITY,
};

/// Default validity window for a NON-move action: a few seconds is ample for the
/// command to reach the owner and execute, while still bounding replay.
pub const DEFAULT_COMMAND_WINDOW_MS: u64 = 4_000;

/// Hard deadline for a single robot `<package>.status` tool call during an
/// advertisement refresh. The status read runs the addon's wasmtime instance,
/// which may be busy or blocked on its own network call to an unreachable robot;
/// a slow/hung instance must NOT stall the refresh (and, by extension, the mesh).
/// On timeout the robot is treated exactly like the offline path: not advertised.
const STATUS_CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// A robot advertised on the mesh: which logical robot exists and which node owns
/// it (has the physical link + the enabled `[robot]` addon). Compact on purpose —
/// it carries only what a controller needs to resolve the owner, never any
/// safety/secret detail (the owner re-clamps and re-authorizes locally).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvertisedRobot {
    /// The owning addon instance id — the value a controller passes as `robot_id`.
    pub robot_id: String,
    /// Package/base id of the owning addon (e.g. "go2"); informational + lets a
    /// controller address by base id when exactly one instance exists.
    pub package_id: String,
    /// Robot kind from the manifest ("quadruped", "drone", ...); informational.
    pub kind: Option<String>,
    /// Endpoint-id hex of the node that owns this robot.
    pub node_id: String,
    /// Owning organization (tenant) of the robot addon instance. A controller
    /// node must only fetch a remote robot's camera when ITS OWN org matches this
    /// — `camera.read` + trusted-registry membership alone do not scope tenants.
    /// Appended last for wire compat: an old peer's announce decodes with
    /// `org_id` defaulting to empty (`#[serde(default)]`, ciborium APPEND-AT-END
    /// rule), which the requester-side scope check treats as a non-match.
    #[serde(default)]
    pub org_id: String,
    /// The owning node's NODE-LOCAL camera id for this robot's video feed, if it
    /// has one yet. Camera rows are never synced (they are node-local by design),
    /// so a controller node otherwise has no way to know which camera id maps to
    /// a remote robot — carrying it here lets the dashboard tile request the
    /// remote camera through the existing frame_proxy mechanism. `None` when the
    /// robot has not been granted a camera yet.
    pub camera_id: Option<String>,
    /// Connection/telemetry status string from the owning addon's `<pkg>.status`
    /// tool (e.g. "online"). Informational for any node browsing the mesh robot
    /// catalog — ownership still gates on the online check, not this raw value.
    /// Appended for wire compat: an old peer's announce decodes with an empty
    /// status (`#[serde(default)]`, ciborium APPEND-AT-END rule).
    #[serde(default)]
    pub status: String,
    /// Battery percentage (0..100) if the robot reports it, else `None`.
    #[serde(default)]
    pub battery_percent: Option<f32>,
    /// Last measured round-trip latency to the physical robot in ms, if known.
    #[serde(default)]
    pub rtt_ms: Option<u32>,
    /// Capability ids the robot exposes (e.g. "move", "sit"), advertised so a
    /// controller node can present available actions without owning the addon.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Rich capability descriptors (label / risk / param schema) driving a
    /// capability-based control UI. Appended last for wire compat: an old peer's
    /// announce decodes with an empty vec (`#[serde(default)]`, ciborium
    /// APPEND-AT-END rule), and a robot addon that emits no `actions_meta` simply
    /// advertises none — the UI then falls back to the flat capability chips.
    #[serde(default)]
    pub actions_meta: Vec<AdvertisedAction>,
    /// Structured runtime telemetry SNAPSHOT (gait / velocity / IMU / battery
    /// detail) read at the advertisement cadence — NOT a high-rate stream. It is
    /// EXCLUDED from `robots_structurally_equal` so its per-tick jitter (imu, etc.)
    /// never drives an `Updated`-delta broadcast storm; the latest snapshot still
    /// rides along on any structural `Updated` and on the periodic full ANNOUNCE.
    /// Appended last for wire compat: an old peer's announce decodes with `None`
    /// (`#[serde(default)]`, ciborium APPEND-AT-END rule).
    #[serde(default)]
    pub telemetry: Option<RobotTelemetrySnapshot>,
    /// SMALL LiDAR availability snapshot (enabled / available / point count /
    /// resolution / origin / frame_seq / ts) — NOT the point cloud. Like
    /// `telemetry` it is EXCLUDED from `robots_structurally_equal` so its per-frame
    /// `frame_seq`/`point_count` churn never drives an `Updated`-delta storm; the
    /// latest snapshot still rides any structural `Updated` and the periodic full
    /// ANNOUNCE. Appended last for wire compat (`#[serde(default)]`, ciborium
    /// APPEND-AT-END rule): an old peer decodes it as `None`.
    #[serde(default)]
    pub lidar: Option<RobotLidarSnapshot>,
}

/// SMALL LiDAR availability snapshot mirrored from the owning addon's
/// `status.lidar`. NEVER carries the point cloud — only enough for the UI and for
/// a future renderer to know a fresh frame exists (then pull it on demand).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RobotLidarSnapshot {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub point_count: u32,
    #[serde(default)]
    pub resolution: Option<f32>,
    #[serde(default)]
    pub origin: Vec<f64>,
    #[serde(default)]
    pub frame_seq: u64,
    #[serde(default)]
    pub last_update_ts: i64,
}

/// IMU snapshot a robot reports (orientation + temperature). Every field is
/// optional so absence is representable (capability-absent, never fabricated).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RobotImuSnapshot {
    #[serde(default)]
    pub roll: Option<f64>,
    #[serde(default)]
    pub pitch: Option<f64>,
    #[serde(default)]
    pub yaw: Option<f64>,
    #[serde(default)]
    pub quaternion: Vec<f64>,
    #[serde(default)]
    pub temperature: Option<f64>,
}

/// Battery detail beyond the flat percentage: voltage / current / cell SOC /
/// pack temperature. All optional.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RobotBatterySnapshot {
    #[serde(default)]
    pub soc: Option<f64>,
    #[serde(default)]
    pub voltage: Option<f64>,
    #[serde(default)]
    pub current: Option<f64>,
    #[serde(default)]
    pub temperature: Option<f64>,
}

/// Structured runtime telemetry snapshot, mirrored from the owning addon's
/// `status.telemetry`. Every field optional / a possibly-empty vector so a robot
/// that omits a value simply leaves it out.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RobotTelemetrySnapshot {
    #[serde(default)]
    pub mode: Option<i64>,
    #[serde(default)]
    pub gait_type: Option<i64>,
    #[serde(default)]
    pub body_height: Option<f64>,
    #[serde(default)]
    pub vx: Option<f64>,
    #[serde(default)]
    pub vy: Option<f64>,
    #[serde(default)]
    pub vyaw: Option<f64>,
    #[serde(default)]
    pub position: Vec<f64>,
    #[serde(default)]
    pub foot_force: Vec<f64>,
    #[serde(default)]
    pub imu: Option<RobotImuSnapshot>,
    #[serde(default)]
    pub battery: Option<RobotBatterySnapshot>,
}

/// One numeric parameter of a parametered robot action, with the inclusive range
/// the UI bounds its input to (the owner re-clamps on receipt regardless).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvertisedActionParam {
    pub name: String,
    pub min: f64,
    pub max: f64,
}

/// Rich descriptor of ONE advertised robot control, mirrored from the owning
/// addon's `<pkg>.status` `actions_meta`. Carries the human label, risk tier and
/// param schema so a controller node can render a capability-driven UI and gate
/// high-risk acrobatics without hardcoding any per-robot action list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvertisedAction {
    pub kind: String,
    pub label: String,
    pub risk: String,
    #[serde(default)]
    pub acrobatic: bool,
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub params: Vec<AdvertisedActionParam>,
}

/// CBOR wire payload for the robot advertisement broadcast: one node's complete
/// set of owned robots. Mirrors the services-announce shape (full snapshot, not a
/// delta) so the receiving registry can `replace_node` idempotently.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RobotsAnnouncePayload {
    pub from_node_id: String,
    pub robots: Vec<AdvertisedRobot>,
}

/// Pull request: a newly-connected peer asks for our complete owned-robot set.
/// Mirrors `MeshServicesGetPayload` — the responder replies with a full snapshot
/// (`RobotsGetResponsePayload`), which the requester `replace_node`s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RobotsGetPayload {
    pub from_node_id: String,
}

/// Response to a `RobotsGetPayload` — the full set of robots THIS node owns.
/// Mirrors `MeshServicesGetResponsePayload` (full snapshot, idempotent
/// `replace_node`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RobotsGetResponsePayload {
    pub from_node_id: String,
    pub robots: Vec<AdvertisedRobot>,
}

/// Push delta — emitted immediately after the local owned-robot set changes.
/// Mirrors `MeshServicesUpdatePayload`: the receiver applies `change` to that
/// node's view via `MeshRobotRegistry::apply_change`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RobotsUpdatePayload {
    pub from_node_id: String,
    pub change: RobotChange,
}

/// An incremental change to one node's advertised robots. Mirrors
/// `tentaflow_protocol::message_body::ServiceChange` (Added/Updated/Removed),
/// keyed by the robot's instance id string for the Removed case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RobotChange {
    Added(AdvertisedRobot),
    Updated(AdvertisedRobot),
    Removed(String),
}

/// In-memory aggregator of robots advertised by every reachable mesh node plus
/// the local node. Mirrors `MeshServicesRegistry` deliberately — its own
/// isolated store, so robot advertisement never pollutes the inference-services
/// catalog / routing. Readers (the resolver) operate purely on a merged snapshot.
#[derive(Default)]
pub struct MeshRobotRegistry {
    /// `node_id` → that node's advertised robots. The local node's own entry is
    /// kept here too (under the local node id) via `replace_local`.
    by_node: RwLock<HashMap<String, Vec<AdvertisedRobot>>>,
}

impl MeshRobotRegistry {
    pub fn new() -> Self {
        Self {
            by_node: RwLock::new(HashMap::new()),
        }
    }

    /// Replace this node's owned-robot list (called by the advertiser after
    /// enumerating local robot addons).
    pub fn replace_local(&self, local_node_id: &str, robots: Vec<AdvertisedRobot>) {
        self.replace_node(local_node_id, robots);
    }

    /// Replace one node's advertised robots (local OR a peer announce). An empty
    /// vector drops the node's entry so a peer that stopped owning robots stops
    /// resolving.
    pub fn replace_node(&self, node_id: &str, robots: Vec<AdvertisedRobot>) {
        let mut g = self.by_node.write();
        if robots.is_empty() {
            g.remove(node_id);
        } else {
            g.insert(node_id.to_string(), robots);
        }
    }

    /// Apply an incremental change to `node_id`'s advertised robots. Mirrors
    /// `MeshServicesRegistry::apply_change`: `Added`/`Updated` upsert by
    /// `robot_id` (creating the node entry if absent — a delta may arrive before
    /// any full snapshot), `Removed` filters by `robot_id` and drops the node
    /// entry entirely once it holds no robots (so a node that lost its last robot
    /// stops resolving).
    pub fn apply_change(&self, node_id: &str, change: RobotChange) {
        let mut g = self.by_node.write();
        match change {
            RobotChange::Added(robot) | RobotChange::Updated(robot) => {
                let entry = g.entry(node_id.to_string()).or_default();
                if let Some(slot) = entry.iter_mut().find(|r| r.robot_id == robot.robot_id) {
                    *slot = robot;
                } else {
                    entry.push(robot);
                }
            }
            RobotChange::Removed(robot_id) => {
                if let Some(entry) = g.get_mut(node_id) {
                    entry.retain(|r| r.robot_id != robot_id);
                    if entry.is_empty() {
                        g.remove(node_id);
                    }
                }
            }
        }
    }

    /// Drop a node's advertisements (peer disconnected / trust revoked).
    pub fn remove_node(&self, node_id: &str) {
        self.by_node.write().remove(node_id);
    }

    /// Flat snapshot of every advertised robot across all known nodes.
    pub fn all(&self) -> Vec<AdvertisedRobot> {
        self.by_node.read().values().flatten().cloned().collect()
    }

    /// Snapshot of the robots THIS node owns, read straight from the cached local
    /// entry the advertiser refreshes every ~10 s. Serves the `RobotsGet` reply
    /// without re-running any addon status tool, so a trusted peer's GET is a cheap
    /// in-memory read and cannot drive per-request status probes.
    pub fn local_robots(&self, local_node_id: &str) -> Vec<AdvertisedRobot> {
        self.by_node
            .read()
            .get(local_node_id)
            .cloned()
            .unwrap_or_default()
    }
}

/// Process-global robot registry. Self-contained (like `node_info_collector`'s
/// module state) so robot discovery needs no `AppState`/`ServiceManager`
/// plumbing change in this sub-chunk; the announce handler and the advertiser
/// both reach it through `global()`.
pub fn global() -> &'static MeshRobotRegistry {
    static REG: OnceLock<MeshRobotRegistry> = OnceLock::new();
    REG.get_or_init(MeshRobotRegistry::new)
}

/// Build this node's advertisement from its enabled robot addons and publish it
/// into the global registry under `local_node_id`. Returns the list so the caller
/// can also CBOR-encode + broadcast it to trusted peers.
pub async fn refresh_local_advertisement(db: &DbPool, local_node_id: &str) -> Vec<AdvertisedRobot> {
    // Ownership means PHYSICALLY CONNECTED, not merely installed. A node that has
    // the robot addon installed but no live link (e.g. no physical access) must
    // NOT advertise the robot, otherwise it would wrongly resolve itself as owner
    // and never route control to the node that actually holds the connection.
    //
    // The connection state AND the camera id both live in the robot addon's
    // PRIVATE SQLite, which Core does not read directly. Both are resolved through
    // the addon's read-only `<package>.status` tool: its result carries `status`
    // ("online" == connected) and `camera_id`. A robot whose status is not online
    // is dropped from the advertisement entirely. This runs frequently (~every
    // 10 s), so a read-only tool call per owned robot is acceptable.
    let addon_manager = dispatch_context().map(|c| c.addon_manager);
    let candidates = crate::mesh::command_executor::collect_local_robot_addons(db);
    let mut robots: Vec<AdvertisedRobot> = Vec::with_capacity(candidates.len());
    for c in candidates {
        // Without a wired addon manager we cannot read the status tool, so we
        // cannot prove the robot is connected — do not advertise it.
        let Some(am) = addon_manager.as_ref() else {
            break;
        };
        let telemetry = read_robot_status(am, &c.addon_id, &c.package_id).await;
        if !telemetry.is_online {
            continue;
        }
        // A successful online status may still omit camera_id (no camera yet);
        // preserve the last-known one across a transient camera gap so the
        // remote tile does not flicker. A genuine empty camera_id from an
        // online robot is honored.
        let camera_id = telemetry
            .camera_id
            .clone()
            .or_else(|| last_advertised_camera_id(&c.addon_id, local_node_id));
        // Tenant of this robot, read from the running addon instance's
        // `AddonState`. A service/boot-started instance has no user org context
        // (`instance_org_id` is None), and an unscoped install carries an empty
        // org — both fall back to the default org so the robot is visible to the
        // default-org session (the same org a membership-less-default session
        // resolves to). A real multi-org install keeps its explicit org.
        let org_id = am
            .instance_org_id(&c.addon_id)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| crate::services::org::DEFAULT_ORG_ID.to_string());
        robots.push(AdvertisedRobot {
            robot_id: c.addon_id,
            package_id: c.package_id,
            kind: c.kind,
            node_id: local_node_id.to_string(),
            org_id,
            camera_id,
            status: telemetry.status,
            battery_percent: telemetry.battery_percent,
            rtt_ms: telemetry.rtt_ms,
            capabilities: telemetry.capabilities,
            actions_meta: telemetry.actions_meta,
            telemetry: telemetry.telemetry,
            lidar: telemetry.lidar,
        });
    }
    global().replace_local(local_node_id, robots.clone());
    robots
}

/// PURE telemetry extracted from a robot addon's `<pkg>.status` tool result. A
/// failed/timed-out status read is represented by `RobotStatusTelemetry::offline()`
/// (the robot cannot be proven connected, so it must not be advertised as owned).
#[derive(Debug, Clone, PartialEq)]
pub struct RobotStatusTelemetry {
    pub is_online: bool,
    pub status: String,
    pub camera_id: Option<String>,
    pub battery_percent: Option<f32>,
    pub rtt_ms: Option<u32>,
    pub capabilities: Vec<String>,
    pub actions_meta: Vec<AdvertisedAction>,
    pub telemetry: Option<RobotTelemetrySnapshot>,
    pub lidar: Option<RobotLidarSnapshot>,
}

impl RobotStatusTelemetry {
    /// The offline sentinel used whenever the status tool cannot be read.
    fn offline() -> Self {
        Self {
            is_online: false,
            status: String::new(),
            camera_id: None,
            battery_percent: None,
            rtt_ms: None,
            capabilities: Vec::new(),
            actions_meta: Vec::new(),
            telemetry: None,
            lidar: None,
        }
    }
}

/// Read a robot addon's read-only `<package>.status` tool and extract its
/// telemetry (online state, camera id, battery, rtt, capabilities). A tool
/// failure is treated as NOT online (the robot cannot be proven connected, so it
/// must not be advertised as owned) and is logged so a broken status tool is
/// diagnosable. The parse itself is the PURE `parse_status_telemetry` helper.
///
/// `call_tool_preauthorized` is a synchronous, BLOCKING wasmtime call: it can hang
/// if the addon instance is busy (e.g. its `on_tick` is stuck on a network call to
/// an unreachable robot). It therefore runs on the blocking pool under a hard
/// `STATUS_CALL_TIMEOUT`. Timeout OR error → treat the robot as NOT online, so a
/// slow/hung instance can never stall the refresh loop.
async fn read_robot_status(
    addon_manager: &Arc<crate::addon::AddonManager>,
    addon_id: &str,
    package_id: &str,
) -> RobotStatusTelemetry {
    let tool = format!("{package_id}.status");
    let am = addon_manager.clone();
    let addon = addon_id.to_string();
    let tool_name = tool.clone();
    let join = tokio::time::timeout(
        STATUS_CALL_TIMEOUT,
        tokio::task::spawn_blocking(move || {
            am.call_tool_system(&addon, &tool_name, serde_json::Value::Null)
        }),
    )
    .await;
    match join {
        Ok(Ok(Ok(result))) => parse_status_telemetry(&result),
        Ok(Ok(Err(e))) => {
            warn!(
                addon = %addon_id,
                tool = %tool,
                "robot advertise: status tool failed; treating robot as offline: {e}"
            );
            RobotStatusTelemetry::offline()
        }
        Ok(Err(e)) => {
            warn!(
                addon = %addon_id,
                tool = %tool,
                "robot advertise: status task panicked; treating robot as offline: {e}"
            );
            RobotStatusTelemetry::offline()
        }
        Err(_) => {
            warn!(
                addon = %addon_id,
                tool = %tool,
                "robot advertise: status tool timed out; treating robot as offline"
            );
            RobotStatusTelemetry::offline()
        }
    }
}

/// PURE extraction of telemetry from a `status` tool result. The robot is online
/// iff its `status` field, trimmed and compared case-insensitively, equals
/// "online" (the same gate the go2 addon's `send_sport_gated` uses to decide
/// local-vs-remote). Any other or missing value fails to offline. `battery_pct`
/// is honored only when non-negative (the go2 addon stores -1 for "unknown");
/// `rtt_ms` likewise. `capabilities` is the optional string array.
fn parse_status_telemetry(status: &serde_json::Value) -> RobotStatusTelemetry {
    let raw_status = status
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let is_online = raw_status.eq_ignore_ascii_case("online");
    let battery_percent = status
        .get("battery_pct")
        .and_then(|v| v.as_f64())
        .filter(|n| *n >= 0.0)
        .map(|n| n as f32);
    let rtt_ms = status
        .get("rtt_ms")
        .and_then(|v| v.as_i64())
        .filter(|n| *n >= 0)
        .map(|n| n as u32);
    let capabilities = status
        .get("capabilities")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default();
    let actions_meta = parse_actions_meta(status);
    let telemetry = parse_telemetry_snapshot(status);
    let lidar = parse_lidar_snapshot(status);
    RobotStatusTelemetry {
        is_online,
        status: raw_status,
        camera_id: parse_status_camera_id(status),
        battery_percent,
        rtt_ms,
        capabilities,
        actions_meta,
        telemetry,
        lidar,
    }
}

/// PURE extraction of the SMALL `lidar` availability sub-object from a `status`
/// result. Absence of the whole object → `None` (no LiDAR capability). Within it,
/// each field is read independently; a missing scalar uses the safe absent value
/// (false / 0 / `None` / empty) and is NEVER fabricated. The point cloud is never
/// carried here — only availability metadata.
fn parse_lidar_snapshot(status: &serde_json::Value) -> Option<RobotLidarSnapshot> {
    let l = status.get("lidar")?;
    if !l.is_object() {
        return None;
    }
    let enabled = l.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
    let available = l.get("available").and_then(|v| v.as_bool()).unwrap_or(false);
    let point_count = l
        .get("point_count")
        .and_then(|v| v.as_u64())
        .map(|n| n.min(u32::MAX as u64) as u32)
        .unwrap_or(0);
    let resolution = l
        .get("resolution")
        .and_then(|v| v.as_f64())
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|n| n as f32);
    let origin = parse_fixed_f64_array(l.get("origin"));
    let frame_seq = l.get("frame_seq").and_then(|v| v.as_u64()).unwrap_or(0);
    let last_update_ts = l.get("last_update_ts").and_then(|v| v.as_i64()).unwrap_or(0);
    Some(RobotLidarSnapshot {
        enabled,
        available,
        point_count,
        resolution,
        origin,
        frame_seq,
        last_update_ts,
    })
}

/// PURE all-or-nothing read of a fixed-layout `[a, b, c, ...]` numeric sensor
/// array (position, foot_force, quaternion, …). These arrays carry positional
/// identity (e.g. `foot_force[0]` is a specific foot), so if ANY element is
/// missing/null/non-numeric the WHOLE vector is dropped rather than compacted —
/// a compacted partial would shift indices and corrupt the per-position mapping.
/// Empty when absent, not an array, or any element is non-numeric.
fn parse_fixed_f64_array(v: Option<&serde_json::Value>) -> Vec<f64> {
    let Some(arr) = v.and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(arr.len());
    for elem in arr {
        match elem.as_f64() {
            Some(n) => out.push(n),
            None => return Vec::new(),
        }
    }
    out
}

/// PURE extraction of the structured `telemetry` snapshot from a `status` result.
/// Absence of the whole object → `None` (capability-absent). Within it, every
/// field is read independently: a missing scalar stays `None`, a missing array
/// stays empty — never an invented value. The IMU and battery sub-objects are
/// likewise omitted entirely when neither carries any value.
fn parse_telemetry_snapshot(status: &serde_json::Value) -> Option<RobotTelemetrySnapshot> {
    let t = status.get("telemetry")?;
    if !t.is_object() {
        return None;
    }
    let num = |k: &str| t.get(k).and_then(|v| v.as_f64());
    let int = |k: &str| t.get(k).and_then(|v| v.as_i64());
    let arr = |k: &str| -> Vec<f64> { parse_fixed_f64_array(t.get(k)) };

    let velocity = t.get("velocity");
    let vnum = |k: &str| velocity.and_then(|v| v.get(k)).and_then(|v| v.as_f64());

    let imu = parse_imu_snapshot(t.get("imu"));
    let battery = parse_battery_snapshot(t.get("battery"));

    let snapshot = RobotTelemetrySnapshot {
        mode: int("mode"),
        gait_type: int("gait_type"),
        body_height: num("body_height"),
        vx: vnum("vx"),
        vy: vnum("vy"),
        vyaw: vnum("vyaw"),
        position: arr("position"),
        foot_force: arr("foot_force"),
        imu,
        battery,
    };

    // An object that carried no usable value at all degrades to None so the UI
    // does not render an empty telemetry panel.
    if snapshot == RobotTelemetrySnapshot::default() {
        None
    } else {
        Some(snapshot)
    }
}

/// PURE extraction of the IMU sub-snapshot. `None` when the block is absent or
/// holds no usable value.
fn parse_imu_snapshot(imu: Option<&serde_json::Value>) -> Option<RobotImuSnapshot> {
    let imu = imu?;
    let num = |k: &str| imu.get(k).and_then(|v| v.as_f64());
    let snapshot = RobotImuSnapshot {
        roll: num("roll"),
        pitch: num("pitch"),
        yaw: num("yaw"),
        quaternion: parse_fixed_f64_array(imu.get("quaternion")),
        temperature: num("temperature"),
    };
    if snapshot == RobotImuSnapshot::default() {
        None
    } else {
        Some(snapshot)
    }
}

/// PURE extraction of the battery sub-snapshot. `None` when absent / empty.
fn parse_battery_snapshot(battery: Option<&serde_json::Value>) -> Option<RobotBatterySnapshot> {
    let battery = battery?;
    let num = |k: &str| battery.get(k).and_then(|v| v.as_f64());
    let snapshot = RobotBatterySnapshot {
        soc: num("soc"),
        voltage: num("voltage"),
        current: num("current"),
        temperature: num("temperature"),
    };
    if snapshot == RobotBatterySnapshot::default() {
        None
    } else {
        Some(snapshot)
    }
}

/// PURE extraction of the rich `actions_meta` descriptor from a `status` result.
/// Absence is a first-class capability-absent case (an older/other robot addon
/// that emits no rich descriptor) → empty vec, NOT an error. Each entry must have
/// a non-empty `kind` and `label`; malformed entries are skipped individually so
/// one bad entry can't drop the whole set. `risk` defaults to "medium" (the safer
/// "needs care" tier) when absent.
fn parse_actions_meta(status: &serde_json::Value) -> Vec<AdvertisedAction> {
    let Some(arr) = status.get("actions_meta").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|entry| {
            let kind = entry.get("kind").and_then(|v| v.as_str())?.trim();
            let label = entry.get("label").and_then(|v| v.as_str()).unwrap_or(kind);
            if kind.is_empty() {
                return None;
            }
            let risk = entry
                .get("risk")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or("medium")
                .to_string();
            let acrobatic = entry.get("acrobatic").and_then(|v| v.as_bool()).unwrap_or(false);
            let read_only = entry.get("read_only").and_then(|v| v.as_bool()).unwrap_or(false);
            let params = entry
                .get("params")
                .and_then(|v| v.as_array())
                .map(|ps| {
                    ps.iter()
                        .filter_map(|p| {
                            let name = p.get("name").and_then(|v| v.as_str())?.trim();
                            if name.is_empty() {
                                return None;
                            }
                            Some(AdvertisedActionParam {
                                name: name.to_string(),
                                min: p.get("min").and_then(|v| v.as_f64()).unwrap_or(f64::NEG_INFINITY),
                                max: p.get("max").and_then(|v| v.as_f64()).unwrap_or(f64::INFINITY),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(AdvertisedAction {
                kind: kind.to_string(),
                label: label.to_string(),
                risk,
                acrobatic,
                read_only,
                params,
            })
        })
        .collect()
}

/// Last camera_id THIS node advertised for `addon_id` (its robot_id), read from
/// the LOCAL entries of the global registry only. Used to survive a transient
/// status failure without dropping the remote tile. The node filter is mandatory:
/// the global registry also holds peers' robots, and a remote entry with the same
/// robot_id must never be preserved as this node's advertised camera_id (that
/// would feed `local_advertised_robot_cameras` and weaken the owner-side
/// allowlist). `refresh_local_advertisement` writes local entries with
/// `node_id == local_node_id`, so filtering on it is sufficient and exact.
fn last_advertised_camera_id(addon_id: &str, local_node_id: &str) -> Option<String> {
    global()
        .all()
        .into_iter()
        .find(|r| r.robot_id == addon_id && r.node_id == local_node_id)
        .and_then(|r| r.camera_id)
}

/// Sort an advertised-robot list by a stable key (`robot_id`, then `node_id`) so
/// the change-detection comparison in the periodic broadcaster is order-insensitive.
/// `collect_local_robot_addons` does not guarantee a stable order, so comparing the
/// raw `Vec` would spuriously detect a "change" every cycle and rebroadcast to all
/// peers. Sorting both the new and the last-broadcast snapshot makes the comparison
/// a set comparison without allocating a set. PURE.
pub fn sort_advertised(mut robots: Vec<AdvertisedRobot>) -> Vec<AdvertisedRobot> {
    robots.sort_by(|a, b| {
        a.robot_id
            .cmp(&b.robot_id)
            .then_with(|| a.node_id.cmp(&b.node_id))
    });
    robots
}

/// PURE structural equality of two advertised robots, EXCLUDING the volatile
/// `rtt_ms`, the high-rate `telemetry` snapshot AND the per-frame `lidar`
/// availability snapshot (its `frame_seq`/`point_count`/`last_update_ts` change on
/// every voxel frame). Used as the `Updated`-delta trigger so neither RTT jitter,
/// per-tick telemetry churn (imu/velocity/foot force) nor LiDAR frame churn alone
/// re-advertises. All three are still CARRIED in
/// the struct (the dashboard shows them) and refreshed mesh-wide by the periodic
/// anti-drift full ANNOUNCE; they just must not drive a per-tick `ROBOTS_UPDATE`
/// broadcast storm given the ~10 s advertiser cadence. Every other field
/// (id/node/org/camera/status/battery/capabilities) is structural: a real change
/// in any of them re-advertises. `battery_percent` (the coarse bucket) changes
/// slowly, so keeping it in the trigger is safe.
fn robots_structurally_equal(a: &AdvertisedRobot, b: &AdvertisedRobot) -> bool {
    a.robot_id == b.robot_id
        && a.package_id == b.package_id
        && a.kind == b.kind
        && a.node_id == b.node_id
        && a.org_id == b.org_id
        && a.camera_id == b.camera_id
        && a.status == b.status
        && a.battery_percent == b.battery_percent
        && a.capabilities == b.capabilities
        && a.actions_meta == b.actions_meta
}

/// PURE diff of two advertised-robot snapshots keyed by `robot_id`. Produces the
/// minimal `RobotChange` set the advertiser pushes as `MESH_MSG_ROBOTS_UPDATE`
/// deltas instead of a full snapshot:
///   - present in `new` but not `old` → `Added`
///   - present in both but structurally changed → `Updated`
///   - present in `old` but not `new` → `Removed`
/// Unchanged robots produce nothing. Order-insensitive (keyed lookup), so the
/// caller does not need to sort first for correctness. The `Updated` trigger uses
/// `robots_structurally_equal` (NOT full `==`): a change in volatile `rtt_ms`
/// alone does not emit a delta, preventing a per-tick broadcast storm. The new
/// `rtt_ms` still rides along on any structural `Updated` and on the periodic full
/// ANNOUNCE.
pub fn diff_advertised(old: &[AdvertisedRobot], new: &[AdvertisedRobot]) -> Vec<RobotChange> {
    use std::collections::HashMap;
    let old_by_id: HashMap<&str, &AdvertisedRobot> =
        old.iter().map(|r| (r.robot_id.as_str(), r)).collect();
    let new_by_id: HashMap<&str, &AdvertisedRobot> =
        new.iter().map(|r| (r.robot_id.as_str(), r)).collect();

    let mut changes = Vec::new();
    for robot in new {
        match old_by_id.get(robot.robot_id.as_str()) {
            None => changes.push(RobotChange::Added(robot.clone())),
            Some(prev) if !robots_structurally_equal(prev, robot) => {
                changes.push(RobotChange::Updated(robot.clone()))
            }
            Some(_) => {}
        }
    }
    for robot in old {
        if !new_by_id.contains_key(robot.robot_id.as_str()) {
            changes.push(RobotChange::Removed(robot.robot_id.clone()));
        }
    }
    changes
}

/// PURE extraction of a non-empty `camera_id` from a `status` tool result.
fn parse_status_camera_id(status: &serde_json::Value) -> Option<String> {
    status
        .get("camera_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Process-global handle to the pieces `dispatch_robot_action` needs that are NOT
/// on a WASM host-function's `AddonState` (the iroh mesh manager, the addon
/// manager, and this node's id). Wired once at startup next to the mesh command
/// executor's `ServiceActionContext`, so the `robot_dispatch_v1` host function
/// can route a controller action without threading AppState into every host call.
#[derive(Clone)]
pub struct RobotDispatchContext {
    pub iroh: Arc<crate::mesh::iroh_manager::IrohMeshManager>,
    pub addon_manager: Arc<crate::addon::AddonManager>,
    pub local_node_id: String,
}

static DISPATCH_CTX: OnceLock<RwLock<Option<RobotDispatchContext>>> = OnceLock::new();

fn dispatch_ctx_cell() -> &'static RwLock<Option<RobotDispatchContext>> {
    DISPATCH_CTX.get_or_init(|| RwLock::new(None))
}

/// Install the global dispatch context (startup wiring). Replaces any prior value
/// so a re-init (e.g. test harness) does not leave a stale node id behind.
pub fn set_dispatch_context(ctx: RobotDispatchContext) {
    *dispatch_ctx_cell().write() = Some(ctx);
}

/// Snapshot of the dispatch context, or `None` before startup wired it.
pub fn dispatch_context() -> Option<RobotDispatchContext> {
    dispatch_ctx_cell().read().clone()
}

/// High-level entry used by the `robot_dispatch_v1` host function: build the real
/// `MeshRobotSender` from the global context and run the shared router. Returns
/// `None` only when the context is not wired yet (caller maps that to an ABI
/// failure) — every robot-level outcome is a `Some(RobotControlResponse)`.
pub async fn dispatch_robot_action_global(
    action: RobotAction,
    robot_id: &str,
    actor_user_id: &str,
    org_id: &str,
    db: &DbPool,
) -> Option<RobotControlResponse> {
    let ctx = dispatch_context()?;
    let sender = MeshRobotSender::new(ctx.iroh.clone());
    Some(
        dispatch_robot_action(
            action,
            robot_id,
            actor_user_id,
            org_id,
            db,
            &ctx.addon_manager,
            &ctx.local_node_id,
            &sender,
        )
        .await,
    )
}

/// Where a `robot_id` lives. `Local` → this node owns it; `Remote` → a single
/// other node advertises it; `Unknown` → nobody advertises it, OR it is
/// ambiguous (2+ different nodes advertise the same id — never guess).
#[derive(Debug, Clone, PartialEq)]
pub enum RobotOwner {
    Local,
    Remote(String),
    Unknown,
}

/// PURE owner selection over a list of advertised robots (no mesh, no DB). Used
/// by `resolve_robot_owner` and directly unit-testable.
///
/// 1. If the LOCAL node advertises `robot_id` → `Local`.
/// 2. Else collect the DISTINCT remote node ids advertising it (exact instance id
///    OR unambiguous package/base id). Exactly one → `Remote`. None → `Unknown`.
///    2+ distinct nodes → `Unknown` (ambiguous; the caller warns).
pub fn select_robot_owner(
    advertised: &[AdvertisedRobot],
    robot_id: &str,
    local_node_id: &str,
) -> RobotOwner {
    let matches = |r: &AdvertisedRobot| -> bool {
        if r.robot_id == robot_id {
            return true;
        }
        let base = if r.package_id.is_empty() {
            r.robot_id.as_str()
        } else {
            r.package_id.as_str()
        };
        base == robot_id
    };

    if advertised
        .iter()
        .any(|r| r.node_id == local_node_id && matches(r))
    {
        return RobotOwner::Local;
    }

    let mut remote_nodes: Vec<&str> = advertised
        .iter()
        .filter(|r| r.node_id != local_node_id && matches(r))
        .map(|r| r.node_id.as_str())
        .collect();
    remote_nodes.sort_unstable();
    remote_nodes.dedup();

    match remote_nodes.as_slice() {
        [only] => RobotOwner::Remote((*only).to_string()),
        [] => RobotOwner::Unknown,
        _ => RobotOwner::Unknown,
    }
}

/// Resolve the owner of `robot_id`. Ownership means PHYSICALLY CONNECTED, which
/// is surfaced as an advertisement: a node only advertises a robot it can prove is
/// online (see `refresh_local_advertisement`). So ownership is decided PURELY over
/// the advertised registry via `select_robot_owner` — `Local` iff THIS node
/// advertises the robot online, never merely because the addon is installed (an
/// installed-but-offline node must route control to whoever holds the link). An
/// ambiguous match (2+ nodes advertise the same id) warns and returns `Unknown`.
///
/// `db` is unused here intentionally: the installed-based DB check is the wrong
/// signal for ownership; the INSTALLED addon is still resolved later by the
/// Local-execute path and the receiver (which run only after ownership is known).
pub fn resolve_robot_owner(_db: &DbPool, local_node_id: &str, robot_id: &str) -> RobotOwner {
    let advertised = global().all();
    let owner = select_robot_owner(&advertised, robot_id, local_node_id);
    if owner == RobotOwner::Unknown {
        let remote = advertised
            .iter()
            .filter(|r| r.node_id != local_node_id)
            .filter(|r| {
                r.robot_id == robot_id
                    || (if r.package_id.is_empty() {
                        r.robot_id.as_str()
                    } else {
                        r.package_id.as_str()
                    }) == robot_id
            })
            .count();
        if remote > 1 {
            warn!(
                robot = %robot_id,
                "robot dispatch: multiple nodes advertise the same robot id; refusing to pick one"
            );
        }
    }
    owner
}

/// Wall-clock milliseconds since the epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Build a fresh `RobotControlRequest`: a new `command_id` (uuid), `issued_at_ms`
/// = now, an expiry window sized to the action (`Move` ≤ `MAX_MOVE_DURATION_MS`,
/// others `DEFAULT_COMMAND_WINDOW_MS`), and the SENDER-side velocity clamp
/// (`action.sanitized(MAX_VELOCITY)`) as defense-in-depth — the receiver clamps
/// again to its own `[robot.safety]`. Returns `Err(RejectReason::Malformed)` if the
/// action carries a non-finite numeric param (NaN/inf is rejected, never coerced),
/// so a bad command is stopped at the sender too. Pure (apart from uuid/clock).
pub fn build_request(
    action: RobotAction,
    robot_id: &str,
    actor_user_id: &str,
    org_id: &str,
) -> Result<RobotControlRequest, RejectReason> {
    let issued_at_ms = now_ms();
    let window = match action {
        RobotAction::Move { .. } => MAX_MOVE_DURATION_MS,
        _ => DEFAULT_COMMAND_WINDOW_MS,
    };
    Ok(RobotControlRequest {
        robot_id: robot_id.to_string(),
        org_id: org_id.to_string(),
        command_id: uuid::Uuid::new_v4().to_string(),
        actor_user_id: actor_user_id.to_string(),
        action: action.sanitized(MAX_VELOCITY)?,
        issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(window),
    })
}

/// Force every advertised robot's `node_id` to the transport-authenticated
/// sender. A node only legitimately advertises its OWN robots, so the per-robot
/// `node_id` is never trusted from the wire: a trusted peer could otherwise embed
/// a victim node's id in an `AdvertisedRobot` and have `remote_camera_owner`
/// later trust that self-claimed id as the camera owner. Normalizing at the
/// receive site guarantees the registry can never hold a robot whose `node_id`
/// differs from the peer that announced it. Mirrors `refresh_local_advertisement`,
/// which sets `node_id = local_node_id` for locally owned robots. PURE.
pub fn normalize_advertised_node_id(
    mut robots: Vec<AdvertisedRobot>,
    sender_node_id: &str,
) -> Vec<AdvertisedRobot> {
    for robot in &mut robots {
        robot.node_id = sender_node_id.to_string();
    }
    robots
}

/// Bind an announce to its transport-authenticated sender: a trusted node must
/// not advertise robots on behalf of another node id. Returns the registry key
/// to use (the transport `from_node_id`) only when the self-claimed payload
/// `from_node_id` matches it AND it is not our own echo; otherwise `None` (drop).
/// PURE — mirrors the pipeline receive-handler guard for unit testing.
pub fn bind_announce_sender<'a>(
    payload_from_node_id: &str,
    transport_from_node_id: &'a str,
    local_node_id: &str,
) -> Option<&'a str> {
    if payload_from_node_id == local_node_id {
        return None;
    }
    if payload_from_node_id != transport_from_node_id {
        return None;
    }
    Some(transport_from_node_id)
}

/// Outbound trust gate decision (PURE). Given whether the target node is
/// currently trusted, returns `Some(rejected(UntrustedPeer))` when the send must
/// be refused, or `None` when it may proceed. Keeps the trust→reject mapping
/// unit-testable without a live mesh handle.
pub fn gate_untrusted_send(target_trusted: bool) -> Option<RobotControlResponse> {
    if target_trusted {
        None
    } else {
        Some(RobotControlResponse::rejected(RejectReason::UntrustedPeer))
    }
}

/// The mesh-send dependency, abstracted so the routing decision is testable with
/// a fake. The real impl forwards the CBOR-encoded request to the owning node as
/// `MeshCommandType::RobotControl` and decodes the `RobotControlResult`.
#[async_trait::async_trait]
pub trait RobotCommandSender: Send + Sync {
    /// Send `request` to `node_id`'s receiver and return its response. A
    /// transport-level failure is `Err`; a robot-level refusal is a successful
    /// `RobotControlResponse` carrying `rejected`/`error`.
    async fn send_remote(
        &self,
        node_id: &str,
        request: &RobotControlRequest,
    ) -> anyhow::Result<RobotControlResponse>;
}

/// Production `RobotCommandSender` over the iroh mesh.
pub struct MeshRobotSender {
    mesh: Arc<crate::mesh::iroh_manager::IrohMeshManager>,
}

impl MeshRobotSender {
    pub fn new(mesh: Arc<crate::mesh::iroh_manager::IrohMeshManager>) -> Self {
        Self { mesh }
    }
}

#[async_trait::async_trait]
impl RobotCommandSender for MeshRobotSender {
    async fn send_remote(
        &self,
        node_id: &str,
        request: &RobotControlRequest,
    ) -> anyhow::Result<RobotControlResponse> {
        // Outbound trust gate: never send a robot command to a node we no longer
        // trust. A stale/revoked owner entry (until the registry cleanup lands)
        // must not receive a command — refuse before touching the wire.
        if let Some(rejection) = gate_untrusted_send(self.mesh.is_trusted(node_id)) {
            warn!(node = %node_id, "robot dispatch: target node untrusted — refusing send");
            return Ok(rejection);
        }
        let mut request_cbor = Vec::new();
        ciborium::ser::into_writer(request, &mut request_cbor)
            .map_err(|e| anyhow::anyhow!("encode robot control request: {e}"))?;
        let response = self
            .mesh
            .send_command(
                node_id,
                tentaflow_protocol::mesh::MeshCommandType::RobotControl { request_cbor },
            )
            .await?;
        if !response.ok {
            return Err(anyhow::anyhow!(
                "remote robot control failed: {}",
                response.error.unwrap_or_else(|| "unknown error".to_string())
            ));
        }
        match response.payload {
            tentaflow_protocol::mesh::MeshCommandResponsePayload::RobotControlResult {
                result_cbor,
            } => ciborium::de::from_reader(&result_cbor[..])
                .map_err(|e| anyhow::anyhow!("decode robot control response: {e}")),
            _ => Err(anyhow::anyhow!(
                "remote robot control returned unexpected payload"
            )),
        }
    }
}

/// The core SENDER router. Builds a fresh request, resolves the owner, and:
///   - Local → executes via the SAME local path the receiver uses
///     (`resolve_robot_addon` + `PermissionMatrix::has_permission` + `plan_execution`
///     + `execute_robot_call`), so sender(local) and receiver share ONE impl.
///   - Remote → sends the CBOR request to the owning node via `sender`.
///   - Unknown → `RobotControlResponse::rejected(UnknownRobot)`.
///
/// The local execute runs on a blocking thread (wasmtime). The `sender` is the
/// only mesh dependency, so the whole routing decision is unit-testable with a
/// fake sender + an in-memory advertisement set.
pub async fn dispatch_robot_action(
    action: RobotAction,
    robot_id: &str,
    actor_user_id: &str,
    org_id: &str,
    db: &DbPool,
    addon_manager: &Arc<crate::addon::AddonManager>,
    local_node_id: &str,
    sender: &dyn RobotCommandSender,
) -> RobotControlResponse {
    let request = match build_request(action, robot_id, actor_user_id, org_id) {
        Ok(req) => req,
        Err(reason) => return RobotControlResponse::rejected(reason),
    };

    match resolve_robot_owner(db, local_node_id, robot_id) {
        RobotOwner::Local => {
            execute_local(&request, db, addon_manager).await
        }
        RobotOwner::Remote(node_id) => match sender.send_remote(&node_id, &request).await {
            Ok(resp) => resp,
            Err(e) => RobotControlResponse::failed(e.to_string()),
        },
        RobotOwner::Unknown => RobotControlResponse::rejected(RejectReason::UnknownRobot),
    }
}

/// Execute a request whose robot is owned by THIS node, reusing the receiver's
/// exact resolve → authorize → plan → execute pipeline (no duplication: it calls
/// the same `resolve_robot_addon`, `PermissionMatrix::has_permission`,
/// `plan_execution` and `execute_robot_call`).
async fn execute_local(
    request: &RobotControlRequest,
    db: &DbPool,
    addon_manager: &Arc<crate::addon::AddonManager>,
) -> RobotControlResponse {
    let resolved = crate::mesh::command_executor::resolve_robot_addon(db, &request.robot_id);
    let authorized = crate::services::rbac::permissions::PermissionMatrix::global()
        .has_permission(
            db,
            &request.actor_user_id,
            &request.org_id,
            request.action.required_permission(),
        )
        .unwrap_or(false);

    let plan = match plan_execution(request, resolved.as_ref(), authorized) {
        Ok(plan) => plan,
        Err(reason) => return RobotControlResponse::rejected(reason),
    };

    let addon_manager = addon_manager.clone();
    let plan_for_exec = plan.clone();
    let read_only = request.action.is_read_only();
    let exec = tokio::task::spawn_blocking(move || {
        crate::mesh::robot_control::execute_robot_call(&addon_manager, &plan_for_exec, read_only)
    })
    .await;

    match exec {
        Ok(resp) => resp,
        Err(e) => RobotControlResponse::failed(format!("robot control task failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ad(robot_id: &str, package_id: &str, node_id: &str) -> AdvertisedRobot {
        AdvertisedRobot {
            robot_id: robot_id.to_string(),
            package_id: package_id.to_string(),
            kind: Some("quadruped".to_string()),
            node_id: node_id.to_string(),
            org_id: "org-1".to_string(),
            camera_id: None,
            status: "online".to_string(),
            battery_percent: Some(80.0),
            rtt_ms: Some(12),
            capabilities: vec!["move".to_string(), "sit".to_string()],
            actions_meta: Vec::new(),
            telemetry: None,
            lidar: None,
        }
    }

    // ----- status camera_id extraction (PURE) -----

    #[test]
    fn parse_status_camera_id_present() {
        let status = serde_json::json!({ "status": "online", "camera_id": "cam-uuid" });
        assert_eq!(parse_status_camera_id(&status).as_deref(), Some("cam-uuid"));
    }

    #[test]
    fn parse_status_camera_id_empty_is_none() {
        let status = serde_json::json!({ "camera_id": "" });
        assert_eq!(parse_status_camera_id(&status), None);
    }

    #[test]
    fn parse_status_camera_id_missing_is_none() {
        let status = serde_json::json!({ "status": "offline" });
        assert_eq!(parse_status_camera_id(&status), None);
    }

    // ----- status online + camera parse (PURE) -----

    #[test]
    fn parse_status_online_true_with_camera() {
        let status = serde_json::json!({ "status": "online", "camera_id": "cam-uuid" });
        let t = parse_status_telemetry(&status);
        assert!(t.is_online);
        assert_eq!(t.camera_id.as_deref(), Some("cam-uuid"));
    }

    #[test]
    fn parse_status_online_true_without_camera() {
        let status = serde_json::json!({ "status": "online", "camera_id": "" });
        let t = parse_status_telemetry(&status);
        assert!(t.is_online);
        assert_eq!(t.camera_id, None);
    }

    #[test]
    fn parse_status_offline_is_not_online() {
        let status = serde_json::json!({ "status": "offline", "camera_id": "cam-uuid" });
        // Even with a camera id, an offline robot is not online (not advertised).
        let t = parse_status_telemetry(&status);
        assert!(!t.is_online);
        assert_eq!(t.camera_id.as_deref(), Some("cam-uuid"));
    }

    #[test]
    fn parse_status_connecting_is_not_online() {
        let status = serde_json::json!({ "status": "connecting" });
        assert!(!parse_status_telemetry(&status).is_online);
    }

    #[test]
    fn parse_status_online_is_case_and_whitespace_tolerant() {
        // Mixed case and surrounding whitespace still count as online.
        for s in ["Online", " online ", "ONLINE", "\tOnLiNe\n"] {
            let status = serde_json::json!({ "status": s });
            assert!(
                parse_status_telemetry(&status).is_online,
                "status {s:?} should parse as online"
            );
        }
    }

    #[test]
    fn parse_status_non_online_value_is_offline() {
        // A value that merely contains "online" but is not exactly it stays offline.
        let status = serde_json::json!({ "status": "online-but-degraded" });
        assert!(!parse_status_telemetry(&status).is_online);
    }

    #[test]
    fn parse_status_missing_field_is_not_online() {
        // A status-tool error result (only an "error" key) is not online.
        let status = serde_json::json!({ "error": "robot not found" });
        assert!(!parse_status_telemetry(&status).is_online);
    }

    // ----- resolver selection (PURE) -----

    #[test]
    fn owner_local_wins_over_remote() {
        // Both the local node AND a peer advertise go2 → local wins.
        let advertised = vec![
            ad("go2", "go2", "node-local"),
            ad("go2", "go2", "node-b"),
        ];
        assert_eq!(
            select_robot_owner(&advertised, "go2", "node-local"),
            RobotOwner::Local
        );
    }

    #[test]
    fn owner_single_remote() {
        let advertised = vec![ad("go2-garage", "go2", "node-b")];
        assert_eq!(
            select_robot_owner(&advertised, "go2-garage", "node-local"),
            RobotOwner::Remote("node-b".to_string())
        );
    }

    #[test]
    fn owner_remote_by_package_base_id() {
        // Addressed by base/package id; exactly one remote instance → resolves.
        let advertised = vec![ad("go2-only", "go2", "node-b")];
        assert_eq!(
            select_robot_owner(&advertised, "go2", "node-local"),
            RobotOwner::Remote("node-b".to_string())
        );
    }

    #[test]
    fn owner_none_when_unadvertised() {
        let advertised = vec![ad("spot-1", "spot", "node-b")];
        assert_eq!(
            select_robot_owner(&advertised, "go2", "node-local"),
            RobotOwner::Unknown
        );
    }

    #[test]
    fn owner_ambiguous_two_nodes_is_unknown() {
        // Two DIFFERENT nodes advertise the same robot id → never guess.
        let advertised = vec![
            ad("go2", "go2", "node-b"),
            ad("go2", "go2", "node-c"),
        ];
        assert_eq!(
            select_robot_owner(&advertised, "go2", "node-local"),
            RobotOwner::Unknown
        );
    }

    #[test]
    fn owner_same_node_twice_is_not_ambiguous() {
        // One node advertising the same robot in two snapshots (id + base) is a
        // single owner, not ambiguity.
        let advertised = vec![ad("go2", "go2", "node-b"), ad("go2", "go2", "node-b")];
        assert_eq!(
            select_robot_owner(&advertised, "go2", "node-local"),
            RobotOwner::Remote("node-b".to_string())
        );
    }

    // ----- request building -----

    #[test]
    fn build_request_has_command_id_and_move_window() {
        let req = build_request(
            RobotAction::Move { vx: 0.3, vy: 0.0, vyaw: 0.0 },
            "go2",
            "u1",
            "org-1",
        )
        .unwrap();
        assert!(!req.command_id.is_empty());
        // Move window is bounded by MAX_MOVE_DURATION_MS.
        assert_eq!(req.expires_at_ms - req.issued_at_ms, MAX_MOVE_DURATION_MS);
        assert_eq!(req.robot_id, "go2");
        assert_eq!(req.actor_user_id, "u1");
        assert_eq!(req.org_id, "org-1");
    }

    #[test]
    fn build_request_non_move_uses_default_window() {
        let req = build_request(RobotAction::Sit, "go2", "u1", "org-1").unwrap();
        assert_eq!(req.expires_at_ms - req.issued_at_ms, DEFAULT_COMMAND_WINDOW_MS);
    }

    #[test]
    fn build_request_clamps_move_velocity_sender_side() {
        // Sender-side defense-in-depth: out-of-range velocity is clamped to the
        // protocol ceiling before it ever hits the wire.
        let req = build_request(
            RobotAction::Move { vx: 9.0, vy: -9.0, vyaw: 0.5 },
            "go2",
            "u1",
            "org-1",
        )
        .unwrap();
        assert_eq!(
            req.action,
            RobotAction::Move {
                vx: MAX_VELOCITY,
                vy: -MAX_VELOCITY,
                vyaw: 0.5
            }
        );
    }

    #[test]
    fn build_request_unique_command_ids() {
        let a = build_request(RobotAction::Stop, "go2", "u1", "org-1").unwrap();
        let b = build_request(RobotAction::Stop, "go2", "u1", "org-1").unwrap();
        assert_ne!(a.command_id, b.command_id);
    }

    // ----- routing decision via a fake sender -----

    /// Fake sender: records the node it was asked to reach and returns a canned
    /// response, so the local/remote/unknown decision is observable without mesh.
    struct FakeSender {
        sent_to: std::sync::Mutex<Option<String>>,
        reply: RobotControlResponse,
    }

    impl FakeSender {
        fn new(reply: RobotControlResponse) -> Self {
            Self {
                sent_to: std::sync::Mutex::new(None),
                reply,
            }
        }
        fn target(&self) -> Option<String> {
            self.sent_to.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl RobotCommandSender for FakeSender {
        async fn send_remote(
            &self,
            node_id: &str,
            _request: &RobotControlRequest,
        ) -> anyhow::Result<RobotControlResponse> {
            *self.sent_to.lock().unwrap() = Some(node_id.to_string());
            Ok(self.reply.clone())
        }
    }

    #[test]
    fn route_decision_remote_sends_to_owner() {
        // The PURE decision says Remote(node-b); the router would forward to the
        // fake sender, which records node-b. We exercise the decision + send leg
        // directly (no DB) to keep this a pure routing test.
        let advertised = vec![ad("go2", "go2", "node-b")];
        let owner = select_robot_owner(&advertised, "go2", "node-local");
        assert_eq!(owner, RobotOwner::Remote("node-b".to_string()));

        let fake = FakeSender::new(RobotControlResponse::ok());
        let req = build_request(RobotAction::Sit, "go2", "u1", "org-1").unwrap();
        let resp = futures::executor::block_on(async {
            if let RobotOwner::Remote(node) = owner {
                fake.send_remote(&node, &req).await.unwrap()
            } else {
                unreachable!()
            }
        });
        assert!(resp.ok);
        assert_eq!(fake.target().as_deref(), Some("node-b"));
    }

    #[test]
    fn route_decision_unknown_rejects_without_sending() {
        let advertised = vec![ad("spot-1", "spot", "node-b")];
        let owner = select_robot_owner(&advertised, "go2", "node-local");
        assert_eq!(owner, RobotOwner::Unknown);

        let fake = FakeSender::new(RobotControlResponse::ok());
        // Unknown never calls the sender → target stays None and the router would
        // return rejected(UnknownRobot).
        assert_eq!(fake.target(), None);
        let resp = RobotControlResponse::rejected(RejectReason::UnknownRobot);
        assert_eq!(resp.rejected, Some(RejectReason::UnknownRobot));
    }

    // ----- announce transport (cbor roundtrip + receive-handler logic) -----

    #[test]
    fn announce_payload_cbor_roundtrip() {
        // First robot carries a camera_id, second does not — the CBOR form must
        // round-trip both the Some and None cases of the field.
        let mut with_camera = ad("go2-garage", "go2", "node-b");
        with_camera.camera_id = Some("11111111-2222-4333-8444-555555555555".to_string());
        let payload = RobotsAnnouncePayload {
            from_node_id: "node-b".to_string(),
            robots: vec![with_camera, ad("spot-1", "spot", "node-b")],
        };
        let bytes = crate::mesh::cbor::encode(&payload).expect("encode");
        let back: RobotsAnnouncePayload = crate::mesh::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, payload);
        assert_eq!(
            back.robots[0].camera_id.as_deref(),
            Some("11111111-2222-4333-8444-555555555555")
        );
        assert_eq!(back.robots[1].camera_id, None);
    }

    #[test]
    fn advertised_robot_org_id_roundtrips() {
        let mut robot = ad("go2-garage", "go2", "node-b");
        robot.org_id = "org-acme".to_string();
        let bytes = crate::mesh::cbor::encode(&robot).expect("encode");
        let back: AdvertisedRobot = crate::mesh::cbor::decode(&bytes).expect("decode");
        assert_eq!(back.org_id, "org-acme");
        assert_eq!(back, robot);
    }

    /// Wire-compat: an announce from an OLD peer (no `org_id` field) decodes with
    /// `org_id` defaulting to empty, so a new node never fails to decode it.
    #[test]
    fn advertised_robot_decodes_legacy_without_org_id() {
        #[derive(Serialize)]
        struct LegacyRobot {
            robot_id: String,
            package_id: String,
            kind: Option<String>,
            node_id: String,
            camera_id: Option<String>,
        }
        let legacy = LegacyRobot {
            robot_id: "go2".to_string(),
            package_id: "go2".to_string(),
            kind: Some("quadruped".to_string()),
            node_id: "node-b".to_string(),
            camera_id: Some("cam-x".to_string()),
        };
        let bytes = crate::mesh::cbor::encode(&legacy).expect("encode legacy");
        let back: AdvertisedRobot = crate::mesh::cbor::decode(&bytes).expect("decode legacy");
        assert_eq!(back.org_id, "");
        assert_eq!(back.camera_id.as_deref(), Some("cam-x"));
        assert_eq!(back.robot_id, "go2");
        // New telemetry fields default when absent on the wire (ciborium
        // APPEND-AT-END rule), so a new node never fails to decode an old peer.
        assert_eq!(back.status, "");
        assert_eq!(back.battery_percent, None);
        assert_eq!(back.rtt_ms, None);
        assert!(back.capabilities.is_empty());
    }

    /// Wire-compat: an announce from an OLD peer that carries `org_id` but NOT the
    /// telemetry fields still decodes, with telemetry defaulting. This proves the
    /// append-at-end ordering of the new fields after `camera_id`/`org_id`.
    #[test]
    fn advertised_robot_decodes_legacy_without_telemetry() {
        #[derive(Serialize)]
        struct PreTelemetryRobot {
            robot_id: String,
            package_id: String,
            kind: Option<String>,
            node_id: String,
            org_id: String,
            camera_id: Option<String>,
        }
        let legacy = PreTelemetryRobot {
            robot_id: "go2".to_string(),
            package_id: "go2".to_string(),
            kind: Some("quadruped".to_string()),
            node_id: "node-b".to_string(),
            org_id: "org-acme".to_string(),
            camera_id: None,
        };
        let bytes = crate::mesh::cbor::encode(&legacy).expect("encode");
        let back: AdvertisedRobot = crate::mesh::cbor::decode(&bytes).expect("decode");
        assert_eq!(back.org_id, "org-acme");
        assert_eq!(back.status, "");
        assert_eq!(back.battery_percent, None);
        assert_eq!(back.rtt_ms, None);
        assert!(back.capabilities.is_empty());
    }

    /// New telemetry fields round-trip through CBOR (Some + None + non-empty caps).
    #[test]
    fn advertised_robot_telemetry_roundtrips() {
        let robot = ad("go2", "go2", "node-b");
        let bytes = crate::mesh::cbor::encode(&robot).expect("encode");
        let back: AdvertisedRobot = crate::mesh::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, robot);
        assert_eq!(back.status, "online");
        assert_eq!(back.battery_percent, Some(80.0));
        assert_eq!(back.rtt_ms, Some(12));
        assert_eq!(back.capabilities, vec!["move", "sit"]);
    }

    // ----- telemetry parse (PURE) -----

    #[test]
    fn parse_status_telemetry_full() {
        let status = serde_json::json!({
            "status": "online",
            "camera_id": "cam-1",
            "battery_pct": 73,
            "rtt_ms": 18,
            "capabilities": ["move", "sit", "stand_up"],
        });
        let t = parse_status_telemetry(&status);
        assert!(t.is_online);
        assert_eq!(t.status, "online");
        assert_eq!(t.camera_id.as_deref(), Some("cam-1"));
        assert_eq!(t.battery_percent, Some(73.0));
        assert_eq!(t.rtt_ms, Some(18));
        assert_eq!(t.capabilities, vec!["move", "sit", "stand_up"]);
    }

    #[test]
    fn parse_status_telemetry_parses_actions_meta() {
        let status = serde_json::json!({
            "status": "online",
            "capabilities": ["move", "sit", "front_flip"],
            "actions_meta": [
                { "kind": "sit", "label": "Siad", "risk": "low", "params": [] },
                { "kind": "move", "label": "Ruch", "risk": "medium", "params": [
                    { "name": "vx", "min": -1.0, "max": 1.0 },
                    { "name": "vyaw", "min": -1.0, "max": 1.0 } ] },
                { "kind": "front_flip", "label": "Front Flip", "risk": "high",
                  "acrobatic": true, "params": [] },
                { "kind": "status", "label": "Status", "risk": "low",
                  "read_only": true, "params": [] },
            ],
        });
        let t = parse_status_telemetry(&status);
        assert_eq!(t.actions_meta.len(), 4);

        let sit = &t.actions_meta[0];
        assert_eq!(sit.kind, "sit");
        assert_eq!(sit.label, "Siad");
        assert_eq!(sit.risk, "low");
        assert!(!sit.acrobatic);
        assert!(!sit.read_only);
        assert!(sit.params.is_empty());

        let mv = &t.actions_meta[1];
        assert_eq!(mv.kind, "move");
        assert_eq!(mv.params.len(), 2);
        assert_eq!(mv.params[0].name, "vx");
        assert_eq!(mv.params[0].min, -1.0);
        assert_eq!(mv.params[0].max, 1.0);

        let flip = &t.actions_meta[2];
        assert_eq!(flip.risk, "high");
        assert!(flip.acrobatic);

        assert!(t.actions_meta[3].read_only);
    }

    #[test]
    fn parse_status_telemetry_absent_actions_meta_is_empty() {
        // An older/other robot addon that emits no rich descriptor: capability-
        // absent, not an error. The UI falls back to flat capability chips.
        let status = serde_json::json!({
            "status": "online", "capabilities": ["move", "sit"],
        });
        let t = parse_status_telemetry(&status);
        assert!(t.actions_meta.is_empty());
        assert_eq!(t.capabilities, vec!["move", "sit"]);
    }

    #[test]
    fn parse_actions_meta_skips_malformed_entries_and_defaults_risk() {
        let status = serde_json::json!({
            "actions_meta": [
                { "label": "no kind here", "risk": "low" },
                { "kind": "", "label": "empty kind" },
                { "kind": "hello" },
                { "kind": "euler", "params": [
                    { "min": 0.0, "max": 1.0 },
                    { "name": "roll", "min": -0.5, "max": 0.5 } ] },
            ],
        });
        let metas = parse_actions_meta(&status);
        assert_eq!(metas.len(), 2);
        assert_eq!(metas[0].kind, "hello");
        assert_eq!(metas[0].label, "hello", "label defaults to kind when absent");
        assert_eq!(metas[0].risk, "medium", "risk defaults to medium when absent");
        assert_eq!(metas[1].kind, "euler");
        assert_eq!(metas[1].params.len(), 1, "param without name is skipped");
        assert_eq!(metas[1].params[0].name, "roll");
    }

    #[test]
    fn parse_actions_meta_roundtrips_through_advertised_robot() {
        let mut robot = ad("go2", "go2", "node-b");
        robot.actions_meta = vec![
            AdvertisedAction {
                kind: "sit".to_string(),
                label: "Siad".to_string(),
                risk: "low".to_string(),
                acrobatic: false,
                read_only: false,
                params: vec![],
            },
            AdvertisedAction {
                kind: "front_flip".to_string(),
                label: "Front Flip".to_string(),
                risk: "high".to_string(),
                acrobatic: true,
                read_only: false,
                params: vec![],
            },
        ];
        let bytes = crate::mesh::cbor::encode(&robot).expect("encode");
        let back: AdvertisedRobot = crate::mesh::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, robot);
        assert_eq!(back.actions_meta.len(), 2);
        assert!(back.actions_meta[1].acrobatic);
    }

    #[test]
    fn diff_advertised_actions_meta_change_emits_updated() {
        let old = vec![ad("go2", "go2", "node-a")];
        let mut changed = ad("go2", "go2", "node-a");
        changed.actions_meta = vec![AdvertisedAction {
            kind: "sit".to_string(),
            label: "Siad".to_string(),
            risk: "low".to_string(),
            acrobatic: false,
            read_only: false,
            params: vec![],
        }];
        let new = vec![changed.clone()];
        assert_eq!(diff_advertised(&old, &new), vec![RobotChange::Updated(changed)]);
    }

    #[test]
    fn parse_status_telemetry_absent_telemetry_is_none() {
        // No `telemetry` object at all → None (capability-absent, not an error).
        let status = serde_json::json!({ "status": "online", "capabilities": ["move"] });
        let t = parse_status_telemetry(&status);
        assert_eq!(t.telemetry, None);
    }

    #[test]
    fn parse_status_telemetry_empty_telemetry_object_is_none() {
        // An empty `telemetry` object carries no usable value → None, so the UI
        // never renders an empty panel.
        let status = serde_json::json!({ "status": "online", "telemetry": {} });
        assert_eq!(parse_status_telemetry(&status).telemetry, None);
    }

    // ----- LiDAR availability snapshot (PURE) -----

    #[test]
    fn parse_status_lidar_present() {
        // A representative go2 `status.lidar` sub-object parses into the snapshot.
        let status = serde_json::json!({
            "status": "online",
            "lidar": {
                "enabled": true,
                "available": true,
                "point_count": 4096,
                "resolution": 0.05,
                "origin": [-1.5, -1.5, -0.2],
                "frame_seq": 7,
                "last_update_ts": 1_700_000_000_i64
            }
        });
        let l = parse_status_telemetry(&status).lidar.expect("lidar present");
        assert!(l.enabled);
        assert!(l.available);
        assert_eq!(l.point_count, 4096);
        assert_eq!(l.resolution, Some(0.05));
        assert_eq!(l.origin, vec![-1.5, -1.5, -0.2]);
        assert_eq!(l.frame_seq, 7);
        assert_eq!(l.last_update_ts, 1_700_000_000);
    }

    #[test]
    fn parse_status_lidar_absent_is_none() {
        // No `lidar` object at all → None (capability-absent, not an error).
        let status = serde_json::json!({ "status": "online", "capabilities": ["move"] });
        assert_eq!(parse_status_telemetry(&status).lidar, None);
    }

    #[test]
    fn parse_status_lidar_partial_uses_safe_defaults() {
        // Only `enabled` reported (no frame yet): available=false, point_count=0,
        // resolution/origin absent — never fabricated.
        let status = serde_json::json!({
            "status": "online",
            "lidar": { "enabled": true }
        });
        let l = parse_status_telemetry(&status).lidar.expect("lidar present");
        assert!(l.enabled);
        assert!(!l.available);
        assert_eq!(l.point_count, 0);
        assert_eq!(l.resolution, None);
        assert!(l.origin.is_empty());
        assert_eq!(l.frame_seq, 0);
    }

    #[test]
    fn lidar_snapshot_churn_does_not_emit_updated_delta() {
        // The LiDAR snapshot (frame_seq / point_count / last_update_ts) changes on
        // EVERY voxel frame. Like telemetry, it must NOT drive an `Updated`-delta:
        // two robots identical except for their lidar snapshot are structurally
        // equal, so `diff_advertised` emits nothing.
        let mut old = ad("go2", "go2", "node-a");
        old.lidar = Some(RobotLidarSnapshot {
            enabled: true,
            available: true,
            point_count: 1000,
            resolution: Some(0.05),
            origin: vec![0.0, 0.0, 0.0],
            frame_seq: 5,
            last_update_ts: 100,
        });
        let mut new = old.clone();
        new.lidar = Some(RobotLidarSnapshot {
            enabled: true,
            available: true,
            point_count: 1234, // new frame, different count
            resolution: Some(0.05),
            origin: vec![0.1, 0.0, 0.0],
            frame_seq: 6, // bumped
            last_update_ts: 200,
        });
        assert!(
            robots_structurally_equal(&old, &new),
            "lidar snapshot churn must not change structural equality"
        );
        assert!(
            diff_advertised(&[old], &[new]).is_empty(),
            "lidar frame churn must not emit an Updated delta"
        );
    }

    #[test]
    fn advertised_robot_lidar_roundtrips() {
        let mut robot = ad("go2", "go2", "node-b");
        robot.lidar = Some(RobotLidarSnapshot {
            enabled: true,
            available: true,
            point_count: 2048,
            resolution: Some(0.1),
            origin: vec![-1.0, -1.0, 0.0],
            frame_seq: 3,
            last_update_ts: 1_700_000_000,
        });
        let bytes = crate::mesh::cbor::encode(&robot).expect("encode");
        let back: AdvertisedRobot = crate::mesh::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, robot);
    }

    #[test]
    fn parse_status_telemetry_parses_snapshot_fields() {
        // A representative go2 `status.telemetry` object (the shape the addon emits
        // from rt/sportmodestate + rt/lf/lowstate) parses into the snapshot.
        let status = serde_json::json!({
            "status": "online",
            "telemetry": {
                "mode": 1,
                "gait_type": 3,
                "body_height": 0.32,
                "velocity": { "vx": 0.4, "vy": -0.1, "vyaw": 0.05 },
                "position": [1.0, 2.0, 0.3],
                "foot_force": [120.0, 118.0, 121.0, 119.0],
                "imu": {
                    "roll": 0.01, "pitch": -0.02, "yaw": 1.57,
                    "quaternion": [0.707, 0.0, 0.0, 0.707],
                    "temperature": 41.0
                },
                "battery": { "soc": 73.0, "voltage": 28.4, "current": -2.1, "temperature": 36.0 }
            }
        });
        let t = parse_status_telemetry(&status).telemetry.expect("telemetry present");
        assert_eq!(t.mode, Some(1));
        assert_eq!(t.gait_type, Some(3));
        assert_eq!(t.body_height, Some(0.32));
        assert_eq!(t.vx, Some(0.4));
        assert_eq!(t.vy, Some(-0.1));
        assert_eq!(t.vyaw, Some(0.05));
        assert_eq!(t.position, vec![1.0, 2.0, 0.3]);
        assert_eq!(t.foot_force, vec![120.0, 118.0, 121.0, 119.0]);
        let imu = t.imu.expect("imu present");
        assert_eq!(imu.roll, Some(0.01));
        assert_eq!(imu.yaw, Some(1.57));
        assert_eq!(imu.quaternion, vec![0.707, 0.0, 0.0, 0.707]);
        assert_eq!(imu.temperature, Some(41.0));
        let bat = t.battery.expect("battery present");
        assert_eq!(bat.soc, Some(73.0));
        assert_eq!(bat.voltage, Some(28.4));
        assert_eq!(bat.current, Some(-2.1));
        assert_eq!(bat.temperature, Some(36.0));
    }

    #[test]
    fn parse_status_telemetry_partial_snapshot_omits_absent_fields() {
        // Only velocity + battery soc reported: the rest stay None / empty, never
        // fabricated. The IMU block is absent → None.
        let status = serde_json::json!({
            "status": "online",
            "telemetry": {
                "velocity": { "vx": 0.2 },
                "battery": { "soc": 55.0 }
            }
        });
        let t = parse_status_telemetry(&status).telemetry.expect("telemetry present");
        assert_eq!(t.vx, Some(0.2));
        assert_eq!(t.vy, None);
        assert_eq!(t.mode, None);
        assert!(t.position.is_empty());
        assert!(t.foot_force.is_empty());
        assert_eq!(t.imu, None);
        let bat = t.battery.expect("battery");
        assert_eq!(bat.soc, Some(55.0));
        assert_eq!(bat.voltage, None);
    }

    #[test]
    fn parse_status_telemetry_fixed_array_with_null_element_is_omitted() {
        // A fixed-layout sensor array (foot_force) carries one bad element. The
        // WHOLE array must be dropped (left empty) rather than compacted — a
        // compacted [120,121,119] would shift indices and misattribute the force
        // to the wrong foot. position has a non-numeric element → also dropped.
        // imu.quaternion likewise. Valid scalar/array fields still parse.
        let status = serde_json::json!({
            "status": "online",
            "telemetry": {
                "mode": 1,
                "body_height": 0.32,
                "position": [1.0, "bad", 0.3],
                "foot_force": [120.0, null, 121.0, 119.0],
                "imu": {
                    "roll": 0.01,
                    "quaternion": [0.707, 0.0, null, 0.707],
                    "temperature": 41.0
                }
            }
        });
        let t = parse_status_telemetry(&status).telemetry.expect("telemetry present");
        assert_eq!(t.mode, Some(1));
        assert_eq!(t.body_height, Some(0.32));
        assert!(t.position.is_empty(), "position with non-numeric element must be omitted whole");
        assert!(t.foot_force.is_empty(), "foot_force with null element must be omitted whole");
        let imu = t.imu.expect("imu present (roll/temperature still valid)");
        assert_eq!(imu.roll, Some(0.01));
        assert_eq!(imu.temperature, Some(41.0));
        assert!(imu.quaternion.is_empty(), "quaternion with null element must be omitted whole");
    }

    #[test]
    fn telemetry_change_only_does_not_emit_updated_delta() {
        // Two snapshots differing ONLY in telemetry (and rtt) must NOT trigger an
        // Updated delta: telemetry jitter every tick would otherwise storm the mesh.
        let old = vec![ad("go2", "go2", "node-a")];
        let mut changed = ad("go2", "go2", "node-a");
        changed.rtt_ms = Some(999);
        changed.telemetry = Some(RobotTelemetrySnapshot {
            vx: Some(0.5),
            imu: Some(RobotImuSnapshot { yaw: Some(0.9), ..Default::default() }),
            ..Default::default()
        });
        let new = vec![changed];
        assert!(
            diff_advertised(&old, &new).is_empty(),
            "telemetry/rtt-only change must not re-advertise"
        );
    }

    #[test]
    fn telemetry_rides_along_on_structural_update() {
        // A genuine structural change (status) re-advertises and carries the fresh
        // telemetry snapshot, so the dashboard stays current without a telemetry
        // delta of its own.
        let old = vec![ad("go2", "go2", "node-a")];
        let mut changed = ad("go2", "go2", "node-a");
        changed.status = "degraded".to_string();
        changed.telemetry = Some(RobotTelemetrySnapshot {
            body_height: Some(0.3),
            ..Default::default()
        });
        let new = vec![changed.clone()];
        let changes = diff_advertised(&old, &new);
        assert_eq!(changes, vec![RobotChange::Updated(changed)]);
        match &changes[0] {
            RobotChange::Updated(r) => {
                assert_eq!(r.telemetry.as_ref().unwrap().body_height, Some(0.3))
            }
            other => panic!("expected Updated, got {other:?}"),
        }
    }

    #[test]
    fn negative_battery_keeps_telemetry_battery_detail() {
        // The flat battery_pct is -1 (unknown) but the detailed telemetry battery
        // still reports voltage: the flat field is None while telemetry carries it.
        let status = serde_json::json!({
            "status": "online", "battery_pct": -1,
            "telemetry": { "battery": { "voltage": 27.9 } }
        });
        let t = parse_status_telemetry(&status);
        assert_eq!(t.battery_percent, None);
        assert_eq!(
            t.telemetry.unwrap().battery.unwrap().voltage,
            Some(27.9)
        );
    }

    #[test]
    fn parse_status_telemetry_negative_battery_and_rtt_are_none() {
        // The go2 addon stores -1 for unknown battery/rtt; that must not surface
        // as a bogus negative reading on the wire.
        let status = serde_json::json!({
            "status": "online", "battery_pct": -1, "rtt_ms": -1,
        });
        let t = parse_status_telemetry(&status);
        assert_eq!(t.battery_percent, None);
        assert_eq!(t.rtt_ms, None);
    }

    #[test]
    fn parse_status_telemetry_offline_keeps_raw_status() {
        let status = serde_json::json!({ "status": "connecting" });
        let t = parse_status_telemetry(&status);
        assert!(!t.is_online);
        assert_eq!(t.status, "connecting");
        assert!(t.capabilities.is_empty());
    }

    #[test]
    fn parse_status_telemetry_missing_status_is_offline() {
        let status = serde_json::json!({ "error": "robot not found" });
        let t = parse_status_telemetry(&status);
        assert!(!t.is_online);
        assert_eq!(t.status, "");
    }

    // ----- diff_advertised (PURE) -----

    #[test]
    fn diff_advertised_detects_added() {
        let old = vec![ad("go2", "go2", "node-a")];
        let new = vec![ad("go2", "go2", "node-a"), ad("spot", "spot", "node-a")];
        let changes = diff_advertised(&old, &new);
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], RobotChange::Added(r) if r.robot_id == "spot"));
    }

    #[test]
    fn diff_advertised_detects_removed() {
        let old = vec![ad("go2", "go2", "node-a"), ad("spot", "spot", "node-a")];
        let new = vec![ad("go2", "go2", "node-a")];
        let changes = diff_advertised(&old, &new);
        assert_eq!(changes, vec![RobotChange::Removed("spot".to_string())]);
    }

    #[test]
    fn diff_advertised_detects_updated_on_battery_change() {
        let old = vec![ad("go2", "go2", "node-a")];
        let mut changed = ad("go2", "go2", "node-a");
        changed.battery_percent = Some(40.0);
        let new = vec![changed.clone()];
        let changes = diff_advertised(&old, &new);
        assert_eq!(changes, vec![RobotChange::Updated(changed)]);
    }

    #[test]
    fn diff_advertised_rtt_only_change_emits_nothing() {
        // RTT jitter every ~10 s must NOT trigger an Updated delta, or the
        // advertiser would broadcast ROBOTS_UPDATE on every tick.
        let old = vec![ad("go2", "go2", "node-a")];
        let mut jittered = ad("go2", "go2", "node-a");
        jittered.rtt_ms = Some(999);
        let new = vec![jittered];
        assert!(diff_advertised(&old, &new).is_empty());
    }

    #[test]
    fn diff_advertised_status_change_emits_updated() {
        let old = vec![ad("go2", "go2", "node-a")];
        let mut changed = ad("go2", "go2", "node-a");
        changed.status = "degraded".to_string();
        let new = vec![changed.clone()];
        assert_eq!(diff_advertised(&old, &new), vec![RobotChange::Updated(changed)]);
    }

    #[test]
    fn diff_advertised_camera_change_emits_updated() {
        let old = vec![ad("go2", "go2", "node-a")];
        let mut changed = ad("go2", "go2", "node-a");
        changed.camera_id = Some("cam-7".to_string());
        let new = vec![changed.clone()];
        assert_eq!(diff_advertised(&old, &new), vec![RobotChange::Updated(changed)]);
    }

    #[test]
    fn diff_advertised_capability_change_emits_updated() {
        let old = vec![ad("go2", "go2", "node-a")];
        let mut changed = ad("go2", "go2", "node-a");
        changed.capabilities = vec!["move".to_string()];
        let new = vec![changed.clone()];
        assert_eq!(diff_advertised(&old, &new), vec![RobotChange::Updated(changed)]);
    }

    #[test]
    fn diff_advertised_carries_new_rtt_on_structural_update() {
        // A structural change re-advertises and the fresh rtt rides along, so the
        // dashboard's rtt stays current without a dedicated rtt-only delta.
        let old = vec![ad("go2", "go2", "node-a")];
        let mut changed = ad("go2", "go2", "node-a");
        changed.status = "degraded".to_string();
        changed.rtt_ms = Some(77);
        let new = vec![changed.clone()];
        let changes = diff_advertised(&old, &new);
        assert_eq!(changes, vec![RobotChange::Updated(changed)]);
        match &changes[0] {
            RobotChange::Updated(r) => assert_eq!(r.rtt_ms, Some(77)),
            other => panic!("expected Updated, got {other:?}"),
        }
    }

    #[test]
    fn diff_advertised_no_change_is_empty() {
        let old = vec![ad("go2", "go2", "node-a"), ad("spot", "spot", "node-a")];
        // Same set, different order — keyed diff must report nothing.
        let new = vec![ad("spot", "spot", "node-a"), ad("go2", "go2", "node-a")];
        assert!(diff_advertised(&old, &new).is_empty());
    }

    // ----- registry apply_change (mirrors MeshServicesRegistry::apply_change) -----

    #[test]
    fn registry_apply_change_added_and_updated_upsert() {
        let reg = MeshRobotRegistry::new();
        reg.apply_change("node-b", RobotChange::Added(ad("go2", "go2", "node-b")));
        assert_eq!(reg.all().len(), 1);
        // Updated with the same robot_id replaces in place (no duplicate).
        let mut updated = ad("go2", "go2", "node-b");
        updated.battery_percent = Some(10.0);
        reg.apply_change("node-b", RobotChange::Updated(updated.clone()));
        let all = reg.all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].battery_percent, Some(10.0));
    }

    #[test]
    fn registry_apply_change_removed_drops_robot_and_empties_node() {
        let reg = MeshRobotRegistry::new();
        reg.apply_change("node-b", RobotChange::Added(ad("go2", "go2", "node-b")));
        reg.apply_change("node-b", RobotChange::Added(ad("spot", "spot", "node-b")));
        reg.apply_change("node-b", RobotChange::Removed("go2".to_string()));
        let all = reg.all();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].robot_id, "spot");
        // Removing the last robot drops the node entry entirely.
        reg.apply_change("node-b", RobotChange::Removed("spot".to_string()));
        assert!(reg.all().is_empty());
    }

    #[test]
    fn registry_local_robots_filters_to_local_node() {
        let reg = MeshRobotRegistry::new();
        reg.replace_node("node-a", vec![ad("go2", "go2", "node-a")]);
        reg.replace_node("node-b", vec![ad("spot", "spot", "node-b")]);
        let local = reg.local_robots("node-a");
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].robot_id, "go2");
        assert_eq!(local[0].node_id, "node-a");
        // No local entry yet → empty, not an error.
        assert!(reg.local_robots("node-c").is_empty());
    }

    #[test]
    fn registry_local_robots_carries_org_id() {
        // The cached-snapshot GET must keep org_id so the consumption layer can
        // filter per caller org (mesh layer advertises within the trust domain).
        let reg = MeshRobotRegistry::new();
        reg.replace_node("node-a", vec![ad("go2", "go2", "node-a")]);
        assert_eq!(reg.local_robots("node-a")[0].org_id, "org-1");
    }

    #[test]
    fn robots_get_response_payload_roundtrip() {
        let payload = RobotsGetResponsePayload {
            from_node_id: "node-b".to_string(),
            robots: vec![ad("go2", "go2", "node-b")],
        };
        let bytes = crate::mesh::cbor::encode(&payload).expect("encode");
        let back: RobotsGetResponsePayload = crate::mesh::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, payload);
    }

    #[test]
    fn robots_update_payload_roundtrip() {
        let payload = RobotsUpdatePayload {
            from_node_id: "node-b".to_string(),
            change: RobotChange::Removed("go2".to_string()),
        };
        let bytes = crate::mesh::cbor::encode(&payload).expect("encode");
        let back: RobotsUpdatePayload = crate::mesh::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, payload);
    }

    /// The new triad discriminants are unique and the documented bytes.
    #[test]
    fn robots_triad_discriminants_are_unique() {
        use tentaflow_protocol::mesh as m;
        assert_eq!(m::MESH_MSG_ROBOTS_GET, 0x4F);
        assert_eq!(m::MESH_MSG_ROBOTS_GET_RESPONSE, 0x50);
        assert_eq!(m::MESH_MSG_ROBOTS_UPDATE, 0x51);
        let all = [
            m::MESH_MSG_ROBOTS_ANNOUNCE,
            m::MESH_MSG_ROBOTS_GET,
            m::MESH_MSG_ROBOTS_GET_RESPONSE,
            m::MESH_MSG_ROBOTS_UPDATE,
        ];
        let mut sorted = all.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), all.len(), "robot discriminants must be distinct");
    }

    #[test]
    fn announce_payload_empty_roundtrip() {
        let payload = RobotsAnnouncePayload {
            from_node_id: "node-b".to_string(),
            robots: vec![],
        };
        let bytes = crate::mesh::cbor::encode(&payload).expect("encode");
        let back: RobotsAnnouncePayload = crate::mesh::cbor::decode(&bytes).expect("decode");
        assert_eq!(back, payload);
        assert!(back.robots.is_empty());
    }

    /// Mirrors the pipeline receive-handler logic: a trusted peer's announce
    /// for a DIFFERENT node populates that node's entry via `replace_node`.
    #[test]
    fn receive_handler_replace_node_populates_registry() {
        let reg = MeshRobotRegistry::new();
        let local = "node-local";
        let payload = RobotsAnnouncePayload {
            from_node_id: "node-b".to_string(),
            robots: vec![ad("go2-garage", "go2", "node-b")],
        };
        // Decode -> self-check -> replace_node (the handler body, sans the
        // trust gate which lives in the pipeline).
        assert_ne!(payload.from_node_id, local);
        reg.replace_node(&payload.from_node_id, payload.robots.clone());

        let advertised = reg.all();
        assert_eq!(advertised.len(), 1);
        assert_eq!(
            select_robot_owner(&advertised, "go2-garage", local),
            RobotOwner::Remote("node-b".to_string())
        );
    }

    /// A self-announce (from_node_id == local) is ignored: the handler skips
    /// `replace_node`, so the registry is never overwritten by an echo of our
    /// own broadcast.
    #[test]
    fn receive_handler_ignores_self_announce() {
        let reg = MeshRobotRegistry::new();
        let local = "node-local";
        // Local node owns go2 (set by the advertiser).
        reg.replace_local(local, vec![ad("go2", "go2", local)]);

        let echo = RobotsAnnouncePayload {
            from_node_id: local.to_string(),
            robots: vec![],
        };
        // Handler guard: from_node_id == local -> skip replace_node entirely.
        if echo.from_node_id != local {
            reg.replace_node(&echo.from_node_id, echo.robots);
        }
        // Local entry survives the echo.
        assert_eq!(reg.all().len(), 1);
        assert_eq!(
            select_robot_owner(&reg.all(), "go2", local),
            RobotOwner::Local
        );
    }

    /// The announce discriminant must not collide with any other mesh message
    /// type and must be the documented byte.
    #[test]
    fn announce_discriminant_is_unique() {
        use tentaflow_protocol::mesh as m;
        assert_eq!(m::MESH_MSG_ROBOTS_ANNOUNCE, 0x4E);
        let others = [
            m::MESH_MSG_HEARTBEAT,
            m::MESH_MSG_FORWARD_REQ,
            m::MESH_MSG_MODEL_LIST,
            m::MESH_MSG_NODE_INFO,
            m::MESH_MSG_HELLO,
            m::MESH_MSG_TOPOLOGY_ANNOUNCE,
            m::MESH_MSG_KNOWN_PEERS,
            m::MESH_MSG_PAIRING_REQUEST,
            m::MESH_MSG_PAIRING_CONFIRM,
            m::MESH_MSG_PAIRING_REJECT,
            m::MESH_MSG_TRUST_REVOKED,
            m::MESH_MSG_TRUSTED_KEYS_SYNC,
            m::MESH_MSG_COMMAND,
            m::MESH_MSG_COMMAND_RESPONSE,
            m::MESH_MSG_DEPLOY_PROGRESS,
            m::MESH_MSG_LOG_CHUNK,
            m::MESH_MSG_STORAGE_PROXY_REQUEST,
            m::MESH_MSG_STORAGE_PROXY_RESPONSE,
            m::MESH_MSG_NODE_LEAVING,
            m::MESH_MSG_FORWARD_STREAM_REQ,
            m::MESH_MSG_ALIAS_SYNC,
            m::MESH_MSG_SERVICES_GET,
            m::MESH_MSG_SERVICES_GET_RESPONSE,
            m::MESH_MSG_SERVICES_ANNOUNCE,
            m::MESH_MSG_SERVICES_UPDATE,
            m::MESH_MSG_HMAC_KEYS_SYNC,
            m::MESH_MSG_FRAME_PROXY_REQUEST,
            m::MESH_MSG_FRAME_PROXY_RESPONSE,
            m::MESH_MSG_SYNC_PUSH,
            m::MESH_MSG_SYNC_ACK,
            m::MESH_MSG_SYNC_PULL,
            m::MESH_MSG_SYNC_PULL_RESPONSE,
            m::MESH_MSG_SYNC_SNAPSHOT_PULL,
            m::MESH_MSG_SYNC_SNAPSHOT_RESPONSE,
            m::MESH_MSG_ROUTING_SYNC,
        ];
        assert!(
            !others.contains(&m::MESH_MSG_ROBOTS_ANNOUNCE),
            "MESH_MSG_ROBOTS_ANNOUNCE collides with an existing mesh discriminant"
        );
    }

    // ----- FIX 3: announce identity binding -----

    #[test]
    fn bind_announce_accepts_matching_sender() {
        // payload.from_node_id == transport sender, not local → key on transport.
        assert_eq!(
            bind_announce_sender("node-b", "node-b", "node-local"),
            Some("node-b")
        );
    }

    #[test]
    fn bind_announce_drops_spoofed_sender() {
        // A trusted node-b advertising robots "on behalf of" node-c is dropped.
        assert_eq!(
            bind_announce_sender("node-c", "node-b", "node-local"),
            None
        );
    }

    #[test]
    fn bind_announce_drops_self_echo() {
        // Our own broadcast echoed back is ignored.
        assert_eq!(
            bind_announce_sender("node-local", "node-local", "node-local"),
            None
        );
    }

    #[test]
    fn receive_handler_keys_on_transport_id_not_payload() {
        // The registry must end up keyed by the transport sender. A matching
        // announce populates that node; a spoofed one leaves the registry empty.
        let reg = MeshRobotRegistry::new();
        let local = "node-local";

        let spoof = RobotsAnnouncePayload {
            from_node_id: "node-c".to_string(),
            robots: vec![ad("go2", "go2", "node-c")],
        };
        if let Some(key) = bind_announce_sender(&spoof.from_node_id, "node-b", local) {
            reg.replace_node(key, spoof.robots);
        }
        assert!(reg.all().is_empty(), "spoofed announce must not populate registry");

        let honest = RobotsAnnouncePayload {
            from_node_id: "node-b".to_string(),
            robots: vec![ad("go2-garage", "go2", "node-b")],
        };
        if let Some(key) = bind_announce_sender(&honest.from_node_id, "node-b", local) {
            reg.replace_node(key, honest.robots);
        }
        assert_eq!(
            select_robot_owner(&reg.all(), "go2-garage", local),
            RobotOwner::Remote("node-b".to_string())
        );
    }

    // ----- FIX 2: outbound trust gate -----

    #[test]
    fn untrusted_target_is_rejected_without_sending() {
        let rejection = gate_untrusted_send(false).expect("untrusted must reject");
        assert_eq!(rejection.rejected, Some(RejectReason::UntrustedPeer));
        assert!(!rejection.ok);
    }

    #[test]
    fn trusted_target_passes_the_gate() {
        assert!(gate_untrusted_send(true).is_none());
    }

    // ----- P1-1: per-robot node_id spoofing in RobotsAnnounce -----

    #[test]
    fn normalize_overwrites_spoofed_per_robot_node_id() {
        // A trusted peer "node-b" announces a robot that self-claims a victim
        // node id ("node-victim"). Normalization must overwrite every robot's
        // node_id with the transport-authenticated sender, so the registry can
        // never trust a spoofed owner for `remote_camera_owner`.
        let spoofed = vec![
            ad("go2-garage", "go2", "node-victim"),
            ad("spot-1", "spot", "node-other"),
        ];
        let normalized = normalize_advertised_node_id(spoofed, "node-b");
        assert!(
            normalized.iter().all(|r| r.node_id == "node-b"),
            "every advertised robot must be re-owned to the transport sender"
        );

        // And the resolver then attributes ownership to the real sender, not the
        // spoofed claim.
        assert_eq!(
            select_robot_owner(&normalized, "go2-garage", "node-local"),
            RobotOwner::Remote("node-b".to_string())
        );
    }

    // ----- P1-2: last-known camera lookup is node-scoped -----

    #[test]
    fn last_advertised_camera_id_ignores_remote_entry_with_same_robot_id() {
        // Unique ids so this test does not race other tests sharing `global()`.
        let robot_id = "p1p2-go2";
        let local = "p1p2-node-local";
        let remote = "p1p2-node-remote";

        let with_cam = |node: &str, cam: &str| AdvertisedRobot {
            robot_id: robot_id.to_string(),
            package_id: "go2".to_string(),
            kind: Some("quadruped".to_string()),
            node_id: node.to_string(),
            org_id: "org-1".to_string(),
            camera_id: Some(cam.to_string()),
            status: "online".to_string(),
            battery_percent: None,
            rtt_ms: None,
            capabilities: Vec::new(),
            actions_meta: Vec::new(),
            telemetry: None,
            lidar: None,
        };

        // A remote node advertises the SAME robot_id with its own camera id.
        global().replace_node(remote, vec![with_cam(remote, "remote-cam")]);
        // The remote entry must NOT be preserved as this node's last-known camera.
        assert_eq!(last_advertised_camera_id(robot_id, local), None);

        // Once this node advertises locally, only the LOCAL camera id is returned.
        global().replace_node(local, vec![with_cam(local, "local-cam")]);
        assert_eq!(
            last_advertised_camera_id(robot_id, local).as_deref(),
            Some("local-cam")
        );

        global().remove_node(local);
        global().remove_node(remote);
    }

    // ----- CHANGE 2: ownership = advertised(online), not installed -----

    /// A node that has the robot addon INSTALLED but does NOT advertise it (robot
    /// offline) must NOT resolve as Local. With a single remote peer advertising
    /// the robot online, ownership resolves to that Remote node. The new
    /// `resolve_robot_owner` is driven purely by the advertised registry, so this
    /// is the exact decision it makes over `global().all()`.
    #[test]
    fn installed_but_not_advertised_resolves_remote() {
        let robot = "chg2-go2";
        let local = "chg2-node-local";
        let remote = "chg2-node-remote";

        // Local node does NOT advertise (installed but robot offline). A remote
        // node advertises it online.
        global().replace_node(remote, vec![ad(robot, "go2", remote)]);
        assert_eq!(
            select_robot_owner(&global().all(), robot, local),
            RobotOwner::Remote(remote.to_string())
        );

        global().remove_node(remote);
    }

    /// When THIS node advertises the robot online, ownership is Local even though
    /// a peer also advertises it (local-wins).
    #[test]
    fn advertised_locally_resolves_local() {
        let robot = "chg2b-go2";
        let local = "chg2b-node-local";
        let remote = "chg2b-node-remote";

        global().replace_node(local, vec![ad(robot, "go2", local)]);
        global().replace_node(remote, vec![ad(robot, "go2", remote)]);
        assert_eq!(
            select_robot_owner(&global().all(), robot, local),
            RobotOwner::Local
        );

        global().remove_node(local);
        global().remove_node(remote);
    }

    /// Nobody advertises the robot (it is offline everywhere) → Unknown, so the
    /// router rejects with UnknownRobot rather than actuating an offline robot.
    #[test]
    fn advertised_nowhere_resolves_unknown() {
        let robot = "chg2c-go2";
        let local = "chg2c-node-local";
        assert_eq!(
            select_robot_owner(&global().all(), robot, local),
            RobotOwner::Unknown
        );
    }

    #[test]
    fn sort_advertised_is_order_insensitive_for_change_detection() {
        // The same set in two different orders must sort to an identical vec, so the
        // periodic broadcaster's `changed` check does not fire on a steady set.
        let a = vec![
            ad("spot-1", "spot", "node-b"),
            ad("go2", "go2", "node-a"),
            ad("go2", "go2", "node-b"),
        ];
        let b = vec![
            ad("go2", "go2", "node-b"),
            ad("spot-1", "spot", "node-b"),
            ad("go2", "go2", "node-a"),
        ];
        let sa = sort_advertised(a);
        let sb = sort_advertised(b);
        assert_eq!(sa, sb, "same set in different orders must sort equal");
        // Sort key is (robot_id, node_id): go2/node-a < go2/node-b < spot-1/node-b.
        assert_eq!(sa[0].robot_id, "go2");
        assert_eq!(sa[0].node_id, "node-a");
        assert_eq!(sa[1].robot_id, "go2");
        assert_eq!(sa[1].node_id, "node-b");
        assert_eq!(sa[2].robot_id, "spot-1");
    }

    #[test]
    fn registry_replace_and_remove_round_trip() {
        let reg = MeshRobotRegistry::new();
        reg.replace_local("node-a", vec![ad("go2", "go2", "node-a")]);
        reg.replace_node("node-b", vec![ad("spot-1", "spot", "node-b")]);
        assert_eq!(reg.all().len(), 2);
        // Empty replace drops the node.
        reg.replace_node("node-b", vec![]);
        assert_eq!(reg.all().len(), 1);
        reg.remove_node("node-a");
        assert!(reg.all().is_empty());
    }
}
