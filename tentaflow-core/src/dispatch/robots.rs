// ===== File: dispatch/robots.rs — binary protocol handlers for the Robots core app =====
//
// Listing + control routing + camera share for robots advertised on the mesh.
// Core stays a DUMB PIPE for robot semantics: all robot logic lives in the go2
// addon and the shared `mesh::robot_control` / `mesh::robot_dispatch` modules.
// This module only:
//   - projects the org-scoped mesh robot registry into `RobotEntry` rows,
//   - maps the protocol `RobotActionWire` onto the core allowlist via the SHARED
//     `RobotAction::from_wire` (same mapping the addon host fn uses) and routes
//     the action through `dispatch_robot_action_global` (Local-execute /
//     Remote-over-mesh — never a duplicate execute path),
//   - exposes a robot's camera to TentaVision (a node-local read grant for a
//     local robot; a clear ok-with-note for a remote robot whose camera rows are
//     node-local and viewed through the cross-node frame path).
//
// Identity (acting user + org) comes from the request `HandlerContext`
// (`org_context`, threaded in by the WS binary entrypoint after session
// resolve), exactly like ml_studio.rs / camera_admin.rs — never fabricated.

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, ProtocolError, RobotActionMeta, RobotActionParam, RobotBatterySnapshot,
    RobotCameraShareResponse, RobotControlResponse, RobotEntry, RobotImuSnapshot,
    RobotLidarStatus, RobotTelemetrySnapshot, RobotsListResponse,
    RobotsPayload,
};

use super::HandlerContext;
use crate::mesh::robot_control::{RejectReason, RobotAction};
use crate::mesh::robot_dispatch::{self, AdvertisedRobot};
use crate::services::rbac::OrgContext;

/// The TentaVision package base id; its enabled instances are the grantees for a
/// local robot camera share so the feed appears in the TentaVision app.
const TENTAVISION_PACKAGE_ID: &str = "tentavision";

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context.as_ref().ok_or_else(|| {
        ProtocolError::new(
            tentaflow_protocol::ProtocolErrorCode::AuthRequired,
            "org context required",
        )
    })
}

/// Project an `AdvertisedRobot` (already org-filtered) into a `RobotEntry`.
fn to_entry(r: AdvertisedRobot, local_node_id: &str) -> RobotEntry {
    let is_local = r.node_id == local_node_id;
    RobotEntry {
        robot_id: r.robot_id,
        owner_node_id: r.node_id,
        is_local,
        kind: r.kind,
        status: r.status,
        battery_percent: r.battery_percent,
        rtt_ms: r.rtt_ms,
        camera_id: r.camera_id,
        capabilities: r.capabilities,
        actions_meta: r
            .actions_meta
            .into_iter()
            .map(|a| RobotActionMeta {
                kind: a.kind,
                label: a.label,
                risk: a.risk,
                acrobatic: a.acrobatic,
                read_only: a.read_only,
                params: a
                    .params
                    .into_iter()
                    .map(|p| RobotActionParam {
                        name: p.name,
                        min: p.min,
                        max: p.max,
                    })
                    .collect(),
            })
            .collect(),
        telemetry: r.telemetry.map(to_telemetry),
        lidar: r.lidar.map(to_lidar),
    }
}

/// Project the mesh-layer `RobotLidarSnapshot` onto the protocol wire type (two
/// distinct types over the same SMALL shape — never the point cloud).
fn to_lidar(l: crate::mesh::robot_dispatch::RobotLidarSnapshot) -> RobotLidarStatus {
    RobotLidarStatus {
        enabled: l.enabled,
        available: l.available,
        point_count: l.point_count,
        resolution: l.resolution,
        origin: l.origin,
        frame_seq: l.frame_seq,
        last_update_ts: l.last_update_ts,
    }
}

/// Project the mesh-layer `RobotTelemetrySnapshot` onto the protocol wire type
/// (two distinct types over the same shape — the mesh struct is the registry's
/// own, the protocol struct is the wire contract).
fn to_telemetry(
    t: crate::mesh::robot_dispatch::RobotTelemetrySnapshot,
) -> RobotTelemetrySnapshot {
    RobotTelemetrySnapshot {
        mode: t.mode,
        gait_type: t.gait_type,
        body_height: t.body_height,
        vx: t.vx,
        vy: t.vy,
        vyaw: t.vyaw,
        position: t.position,
        foot_force: t.foot_force,
        joints: t.joints,
        imu: t.imu.map(|i| RobotImuSnapshot {
            roll: i.roll,
            pitch: i.pitch,
            yaw: i.yaw,
            quaternion: i.quaternion,
            temperature: i.temperature,
        }),
        battery: t.battery.map(|b| RobotBatterySnapshot {
            soc: b.soc,
            voltage: b.voltage,
            current: b.current,
            temperature: b.temperature,
        }),
    }
}

/// Stable refusal tag for the wire response (greppable, language-neutral) — the
/// same tags the addon host fn surfaces.
fn reject_tag(reason: &RejectReason) -> &'static str {
    match reason {
        RejectReason::Expired => "expired",
        RejectReason::FutureDated => "future_dated",
        RejectReason::Malformed => "malformed",
        RejectReason::MoveDurationTooLong => "move_duration_too_long",
        RejectReason::PermissionDenied => "permission_denied",
        RejectReason::EstopActive => "estop_active",
        RejectReason::UntrustedPeer => "untrusted_peer",
        RejectReason::UnknownRobot => "unknown_robot",
    }
}

#[handler(variant = "RobotsListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn robots_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    match req {
        MessageBody::RobotsBody(RobotsPayload::ListRequest(_)) => {}
        _ => return Err(ProtocolError::bad_request("expected RobotsListRequest")),
    }
    let org = require_org(ctx)?;
    let local_node_id = ctx.state.local_node_id.to_string();
    // The mesh registry holds robots from ALL orgs by design (like services);
    // org scoping is enforced HERE, at the consumption layer.
    let robots = robot_dispatch::global()
        .all()
        .into_iter()
        .filter(|r| r.org_id == org.org_id)
        .map(|r| to_entry(r, &local_node_id))
        .collect();
    Ok(MessageBody::RobotsBody(RobotsPayload::ListResponse(
        RobotsListResponse { robots },
    )))
}

#[handler(variant = "RobotControlRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn robots_control(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::RobotsBody(RobotsPayload::ControlRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected RobotControlRequest")),
    };
    let org = require_org(ctx)?;
    // The protocol `RobotActionWire` is one of two wire encodings (ciborium here,
    // minicbor for the host ABI) over the SAME flat shape; run the SINGLE shared
    // `kind`→action allowlist directly. Closed allowlist: an unknown kind is refused.
    let action = RobotAction::from_kind_params(
        &payload.action.kind,
        payload.action.vx,
        payload.action.vy,
        payload.action.vyaw,
        payload.action.p1,
        payload.action.p2,
        payload.action.p3,
        payload.action.p4,
    )
    .ok_or_else(|| ProtocolError::bad_request("unknown robot action kind"))?;

    let db = ctx.state.db.clone();
    let actor_user_id = org.user_id.clone();
    let caller_org = org.org_id.clone();
    let robot_id = payload.robot_id.clone();

    // `dispatch_robot_action_global` resolves the owner and either local-executes
    // (on its own blocking task) or forwards over the mesh; RBAC
    // (`RobotAction::required_permission`) is re-checked by the dispatch + receiver,
    // so e-stop-class still works.
    let resp = robot_dispatch::dispatch_robot_action_global(
        action,
        &robot_id,
        &actor_user_id,
        &caller_org,
        &db,
    )
    .await;

    let wire = match resp {
        Some(r) => RobotControlResponse {
            ok: r.ok,
            rejected: r.rejected.as_ref().map(|reason| reject_tag(reason).to_string()),
            error: r.error,
            // Read-only actions (lidar_frame/status) return their JSON payload here.
            result: r.result_json,
        },
        // Context not wired (mesh not started) — there is no node to route to.
        None => RobotControlResponse {
            ok: false,
            rejected: None,
            error: Some("robot dispatch context unavailable".to_string()),
            result: None,
        },
    };
    Ok(MessageBody::RobotsBody(RobotsPayload::ControlResponse(wire)))
}

#[handler(variant = "RobotCameraShareRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn robots_camera_share(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::RobotsBody(RobotsPayload::CameraShareRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected RobotCameraShareRequest")),
    };
    let org = require_org(ctx)?;
    let db = &ctx.state.db;
    let local_node_id = ctx.state.local_node_id.to_string();

    // Resolve where the robot lives (org-scoped registry view).
    let owner = robot_dispatch::global()
        .all()
        .into_iter()
        .find(|r| r.org_id == org.org_id && r.robot_id == payload.robot_id);
    let Some(robot) = owner else {
        return Ok(MessageBody::RobotsBody(RobotsPayload::CameraShareResponse(
            RobotCameraShareResponse {
                ok: false,
                error: Some("robot not found in this organization".to_string()),
                note: None,
            },
        )));
    };

    // REMOTE robot: camera rows are node-local, so no local read grant is possible.
    // The dashboard tile already views a remote camera through the cross-node frame
    // path (cameraFrameUrlRequest → remote_camera_owner), so return ok-with-note
    // rather than faking a grant.
    if robot.node_id != local_node_id {
        return Ok(MessageBody::RobotsBody(RobotsPayload::CameraShareResponse(
            RobotCameraShareResponse {
                ok: true,
                error: None,
                note: Some(
                    "remote robot camera is viewed via the cross-node frame path; no local grant is created"
                        .to_string(),
                ),
            },
        )));
    }

    let resp = share_local_robot_camera(
        db,
        ctx.state.addon_manager.as_deref(),
        &org.org_id,
        &payload.camera_id,
        &org.user_id,
    );
    Ok(MessageBody::RobotsBody(RobotsPayload::CameraShareResponse(resp)))
}

/// Grant `read` on a node-local robot camera to every enabled TentaVision
/// instance THAT BELONGS TO THE CALLER'S ORG, so the feed appears in the
/// TentaVision app. The `addons` DB row is org-agnostic; the owning org lives on
/// the running instance and is read via `AddonManager::instance_org_id`, so a
/// camera is never granted to a TentaVision instance in another org. The camera
/// must be active in the caller's org; the grant is idempotent.
#[cfg(not(feature = "camera"))]
fn share_local_robot_camera(
    _db: &crate::db::DbPool,
    _addon_manager: Option<&crate::addon::AddonManager>,
    _org_id: &str,
    _camera_id: &str,
    _created_by: &str,
) -> RobotCameraShareResponse {
    RobotCameraShareResponse {
        ok: false,
        error: Some("camera support is not built into this node".to_string()),
        note: None,
    }
}

#[cfg(feature = "camera")]
fn share_local_robot_camera(
    db: &crate::db::DbPool,
    addon_manager: Option<&crate::addon::AddonManager>,
    org_id: &str,
    camera_id: &str,
    created_by: &str,
) -> RobotCameraShareResponse {
    match crate::db::repository::get_camera_in_org(db, camera_id, Some(org_id)) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return RobotCameraShareResponse {
                ok: false,
                error: Some("camera not found in this organization".to_string()),
                note: None,
            };
        }
        Err(e) => {
            return RobotCameraShareResponse {
                ok: false,
                error: Some(format!("camera lookup failed: {e}")),
                note: None,
            };
        }
    }

    // Org scoping requires reading each instance's owning org from its running
    // store; without the AddonManager we cannot tenant-scope and MUST NOT grant
    // to potentially cross-org instances.
    let Some(addon_manager) = addon_manager else {
        return RobotCameraShareResponse {
            ok: false,
            error: Some("addon manager unavailable; cannot scope share to org".to_string()),
            note: None,
        };
    };

    let candidates = match crate::db::repository::list_addons(db) {
        Ok(addons) => addons
            .into_iter()
            .filter(|a| a.is_enabled && a.package_id == TENTAVISION_PACKAGE_ID)
            .map(|a| a.addon_id)
            .collect::<Vec<_>>(),
        Err(e) => {
            return RobotCameraShareResponse {
                ok: false,
                error: Some(format!("addon lookup failed: {e}")),
                note: None,
            };
        }
    };

    // The `addons` row is org-agnostic; the owning org lives on the running
    // instance. Keep only TentaVision instances whose org matches the caller's so
    // a camera is never granted across org boundaries.
    let grantees = candidates
        .into_iter()
        .filter(|addon_id| addon_manager.instance_org_id(addon_id).as_deref() == Some(org_id))
        .collect::<Vec<_>>();

    if grantees.is_empty() {
        return RobotCameraShareResponse {
            ok: false,
            error: Some(
                "no enabled TentaVision instance in this organization to share with".to_string(),
            ),
            note: None,
        };
    }

    for grantee in &grantees {
        if let Err(e) =
            crate::db::repository::grant_camera(db, camera_id, grantee, "read", org_id, created_by)
        {
            return RobotCameraShareResponse {
                ok: false,
                error: Some(format!("grant failed: {e}")),
                note: None,
            };
        }
    }

    RobotCameraShareResponse {
        ok: true,
        error: None,
        note: Some(format!(
            "camera shared with {} TentaVision instance(s)",
            grantees.len()
        )),
    }
}
