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
use std::time::{SystemTime, UNIX_EPOCH};

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
}

/// CBOR wire payload for the robot advertisement broadcast: one node's complete
/// set of owned robots. Mirrors the services-announce shape (full snapshot, not a
/// delta) so the receiving registry can `replace_node` idempotently.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RobotsAnnouncePayload {
    pub from_node_id: String,
    pub robots: Vec<AdvertisedRobot>,
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

    /// Drop a node's advertisements (peer disconnected / trust revoked).
    pub fn remove_node(&self, node_id: &str) {
        self.by_node.write().remove(node_id);
    }

    /// Flat snapshot of every advertised robot across all known nodes.
    pub fn all(&self) -> Vec<AdvertisedRobot> {
        self.by_node.read().values().flatten().cloned().collect()
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
pub fn refresh_local_advertisement(db: &DbPool, local_node_id: &str) -> Vec<AdvertisedRobot> {
    // The camera id lives in the robot addon's PRIVATE SQLite (`robot.camera_id`),
    // which Core does not read directly. Resolve it through the addon's read-only
    // `<package>.status` tool when the dispatch context (addon manager) is wired.
    // This runs ~every 600 heartbeats, so a read-only tool call per owned robot is
    // acceptable. A robot with no camera yet (or no wired context) stays `None`.
    let addon_manager = dispatch_context().map(|c| c.addon_manager);
    let robots: Vec<AdvertisedRobot> = crate::mesh::command_executor::collect_local_robot_addons(db)
        .into_iter()
        .map(|c| {
            let camera_id = addon_manager
                .as_ref()
                .and_then(|am| {
                    resolve_robot_camera_id(am, &c.addon_id, &c.package_id, local_node_id)
                });
            // Tenant of this robot, read from the running addon instance's
            // `AddonState` (the only place a robot addon's org lives — the DB
            // `addons` row is org-agnostic). An instance with no org context
            // (system/boot start) advertises an empty org_id, which the
            // requester-side scope check treats as a non-match.
            let org_id = addon_manager
                .as_ref()
                .and_then(|am| am.instance_org_id(&c.addon_id))
                .unwrap_or_default();
            AdvertisedRobot {
                robot_id: c.addon_id,
                package_id: c.package_id,
                kind: c.kind,
                node_id: local_node_id.to_string(),
                org_id,
                camera_id,
            }
        })
        .collect();
    global().replace_local(local_node_id, robots.clone());
    robots
}

/// Pull a robot addon's current `camera_id` out of its read-only `status` tool.
/// The tool name is `<package_id>.status` (e.g. `go2.status`) and its JSON result
/// carries `camera_id`; an empty string or a missing/non-string field is treated
/// as "no camera yet" (`None`). System-initiated call (advertiser), so it goes
/// through `call_tool_preauthorized` — the advertiser already owns this local
/// addon and the `status` tool is read-only.
///
/// On a TRANSIENT status-tool failure the last-known advertised camera_id for
/// this robot is preserved instead of clearing it to `None` — a clear would make
/// the remote dashboard tile vanish until the next ~600-heartbeat refresh. Only a
/// SUCCESSFUL status call returning an empty camera_id clears it (genuine
/// removal). The failure is logged (warn) so a broken status tool is diagnosable.
fn resolve_robot_camera_id(
    addon_manager: &Arc<crate::addon::AddonManager>,
    addon_id: &str,
    package_id: &str,
    local_node_id: &str,
) -> Option<String> {
    let tool = format!("{package_id}.status");
    match addon_manager.call_tool_preauthorized(
        addon_id,
        &tool,
        serde_json::Value::Null,
        "system",
    ) {
        Ok(result) => parse_status_camera_id(&result),
        Err(e) => {
            let last_known = last_advertised_camera_id(addon_id, local_node_id);
            warn!(
                addon = %addon_id,
                tool = %tool,
                preserved_camera = last_known.is_some(),
                "robot advertise: status tool failed; preserving last-known camera_id: {e}"
            );
            last_known
        }
    }
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

/// Resolve the owner of `robot_id`. Local short-circuits on the DB (the
/// authoritative local check via `resolve_robot_addon`) so a controller that also
/// owns the robot never needs an advertisement to find itself. Otherwise defers
/// to the PURE `select_robot_owner` over the advertised registry; an ambiguous
/// match warns and returns `Unknown` (never actuate the wrong node's robot).
pub fn resolve_robot_owner(db: &DbPool, local_node_id: &str, robot_id: &str) -> RobotOwner {
    if crate::mesh::command_executor::resolve_robot_addon(db, robot_id).is_some() {
        return RobotOwner::Local;
    }
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
/// again to its own `[robot.safety]`. Pure (apart from uuid/clock).
pub fn build_request(
    action: RobotAction,
    robot_id: &str,
    actor_user_id: &str,
    org_id: &str,
) -> RobotControlRequest {
    let issued_at_ms = now_ms();
    let window = match action {
        RobotAction::Move { .. } => MAX_MOVE_DURATION_MS,
        _ => DEFAULT_COMMAND_WINDOW_MS,
    };
    RobotControlRequest {
        robot_id: robot_id.to_string(),
        org_id: org_id.to_string(),
        command_id: uuid::Uuid::new_v4().to_string(),
        actor_user_id: actor_user_id.to_string(),
        action: action.sanitized(MAX_VELOCITY),
        issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(window),
    }
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
    let request = build_request(action, robot_id, actor_user_id, org_id);

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
        );
        assert!(!req.command_id.is_empty());
        // Move window is bounded by MAX_MOVE_DURATION_MS.
        assert_eq!(req.expires_at_ms - req.issued_at_ms, MAX_MOVE_DURATION_MS);
        assert_eq!(req.robot_id, "go2");
        assert_eq!(req.actor_user_id, "u1");
        assert_eq!(req.org_id, "org-1");
    }

    #[test]
    fn build_request_non_move_uses_default_window() {
        let req = build_request(RobotAction::Sit, "go2", "u1", "org-1");
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
        );
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
        let a = build_request(RobotAction::Stop, "go2", "u1", "org-1");
        let b = build_request(RobotAction::Stop, "go2", "u1", "org-1");
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
        let req = build_request(RobotAction::Sit, "go2", "u1", "org-1");
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
