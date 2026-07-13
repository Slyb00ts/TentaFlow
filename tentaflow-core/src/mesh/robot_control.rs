// =============================================================================
// File: mesh/robot_control.rs
// Purpose: Cross-node robot control — the typed, allowlisted action model plus
//          the PURE safety logic (velocity/duration clamp, expiry validation,
//          idempotency dedup, per-action permission mapping) shared by the mesh
//          RobotControl command path. Deliberately I/O-free and vendor-agnostic:
//          there is NO generic "call any addon tool" escape hatch — the only
//          remote-control surface is the `RobotAction` allowlist. The receiver
//          wiring (command_executor) and the go2 tool mapping live in later
//          sub-chunks; this module is fully unit-testable without a robot or a
//          second node.
// =============================================================================

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::json;

/// Hard ceiling on commanded velocity magnitude (normalized protocol range is
/// -1..1). A receiver further clamps to the robot's own `[robot.safety]`.
pub const MAX_VELOCITY: f64 = 1.0;

/// A `Move` may stay in effect at most this long before the robot must be
/// re-commanded — "set velocity until deadline", never open-ended.
pub const MAX_MOVE_DURATION_MS: u64 = 2000;

/// How long a processed command id is remembered for duplicate suppression.
pub const IDEMPOTENCY_TTL_MS: u64 = 30_000;

/// Allowed clock skew for a command's `issued_at_ms` being ahead of the
/// receiver's clock (commands further in the future are rejected as bogus).
pub const MAX_CLOCK_SKEW_MS: u64 = 5_000;

/// Fuel budget for a single robot flow-block invocation on the receiver. Sized
/// to the go2 `[resources].fuel_limit` so a move/pose block runs to completion.
pub const ROBOT_BLOCK_FUEL: u64 = 200_000_000;

/// The COMPLETE remote-control surface. Vendor-agnostic; a concrete robot addon
/// maps these to its own commands (sub-chunk 3). No free-form tool field exists,
/// so trust-paired mesh can never be turned into arbitrary remote addon execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RobotAction {
    /// Set body velocity until the command deadline (NOT incremental). Values are
    /// normalized -1..1 and clamped by `sanitized()`.
    Move {
        vx: f64,
        vy: f64,
        vyaw: f64,
    },
    /// Soft stop (stop current motion). E-stop-class: always allowed through.
    Stop,
    /// Emergency stop + durable safety latch. E-stop-class: always allowed through.
    Estop,
    /// Clear the e-stop latch.
    ResetEstop,
    RecoveryStand,
    StandUp,
    StandDown,
    /// Force-balance standing pose (Go2 BalanceStand).
    BalanceStand,
    Sit,
    Hello,
    Stretch,
    /// Body orientation in radians. Clamped to `EULER_LIMIT` per axis.
    Euler {
        roll: f64,
        pitch: f64,
        yaw: f64,
    },
    /// Body height delta in meters. Clamped to `BODY_HEIGHT_RANGE`.
    BodyHeight {
        height: f64,
    },
    /// Foot lift height in meters (trot gait). Clamped to `FOOT_RAISE_RANGE`.
    FootRaiseHeight {
        height: f64,
    },
    /// Gait speed level: -1 slow, 0 normal, 1 fast. Clamped to [-1, 1] integer.
    SpeedLevel {
        level: f64,
    },
    /// Composite static body pose: an `Euler` orientation plus a `BodyHeight`
    /// delta. Implemented as a composite (Euler+BodyHeight) rather than relying on
    /// the firmware `Pose` toggle id, so it works on every firmware.
    Pose {
        roll: f64,
        pitch: f64,
        yaw: f64,
        height: f64,
    },
    /// Hip wiggle gesture (Go2 WiggleHips).
    WiggleHips,
    /// "Finger heart" gesture (Go2 FingerHeart).
    Heart,
    Dance1,
    Dance2,
    /// High-risk acrobatic: scrape gesture. Air-locked on some firmware.
    Scrape,
    /// High-risk acrobatic: front flip. Air-locked on some firmware.
    FrontFlip,
    /// High-risk acrobatic: front jump. Air-locked on some firmware.
    FrontJump,
    /// High-risk acrobatic: front pounce. Air-locked on some firmware.
    FrontPounce,
    /// Read-only telemetry/status snapshot.
    Status,
    /// Enable the LiDAR sensor (subscribe to the voxel map). Data-path only — does
    /// NOT move hardware, but it is an actuator toggle so it needs `robot.command`.
    LidarOn,
    /// Disable the LiDAR sensor.
    LidarOff,
    /// Enable on-board obstacle avoidance. When active the robot autonomously
    /// avoids obstacles and may refuse manual turns — an actuator toggle, so it
    /// needs `robot.command`.
    ObstacleAvoidOn,
    /// Disable on-board obstacle avoidance (manual-driving default).
    ObstacleAvoidOff,
    /// Read-only fetch of the latest decoded LiDAR frame (points + metadata) for an
    /// on-demand renderer. Reports state only, never moves hardware.
    LidarFrame,
}

/// Per-axis body-orientation clamp (radians). Go2 accepts roughly ±0.75 rad on
/// each Euler axis; clamp conservatively so a bad caller can never command an
/// extreme tilt.
pub const EULER_LIMIT: f64 = 0.75;

/// Body-height delta clamp (meters). Negative lowers the body; positive raises it.
pub const BODY_HEIGHT_MIN: f64 = -0.18;
pub const BODY_HEIGHT_MAX: f64 = 0.03;

/// Foot lift height clamp (meters) during trot.
pub const FOOT_RAISE_MIN: f64 = -0.06;
pub const FOOT_RAISE_MAX: f64 = 0.10;

impl RobotAction {
    /// The SINGLE source of the `kind`→action allowlist, keyed on primitives so
    /// both wire shapes (the minicbor SDK/host ABI `RobotActionWire` and the
    /// ciborium/serde protocol `RobotActionWire`) map through the SAME closed set.
    /// An unknown `kind` returns `None` so every caller refuses it identically;
    /// there is no duplicated tool→action mapping anywhere.
    pub fn from_kind_axes(kind: &str, vx: f64, vy: f64, vyaw: f64) -> Option<RobotAction> {
        Self::from_kind_params(kind, vx, vy, vyaw, 0.0, 0.0, 0.0, 0.0)
    }

    /// The full constructor: the move axes plus four generic numeric params whose
    /// meaning is keyed by `kind` (euler → p1/p2/p3 = roll/pitch/yaw; body_height
    /// → p1 = height; foot_raise_height → p1 = height; speed_level → p1 = level;
    /// pose → p1/p2/p3/p4 = roll/pitch/yaw/height). An unknown `kind` returns
    /// `None` so every caller refuses it identically. Raw values are NOT clamped
    /// here — `sanitized()` is the single clamp point.
    #[allow(clippy::too_many_arguments)]
    pub fn from_kind_params(
        kind: &str,
        vx: f64,
        vy: f64,
        vyaw: f64,
        p1: f64,
        p2: f64,
        p3: f64,
        p4: f64,
    ) -> Option<RobotAction> {
        Some(match kind {
            "move" => RobotAction::Move { vx, vy, vyaw },
            "stop" => RobotAction::Stop,
            "estop" => RobotAction::Estop,
            "reset_estop" => RobotAction::ResetEstop,
            "recovery_stand" => RobotAction::RecoveryStand,
            "stand_up" => RobotAction::StandUp,
            "stand_down" => RobotAction::StandDown,
            "balance_stand" => RobotAction::BalanceStand,
            "sit" => RobotAction::Sit,
            "hello" => RobotAction::Hello,
            "stretch" => RobotAction::Stretch,
            "euler" => RobotAction::Euler {
                roll: p1,
                pitch: p2,
                yaw: p3,
            },
            "body_height" => RobotAction::BodyHeight { height: p1 },
            "foot_raise_height" => RobotAction::FootRaiseHeight { height: p1 },
            "speed_level" => RobotAction::SpeedLevel { level: p1 },
            "pose" => RobotAction::Pose {
                roll: p1,
                pitch: p2,
                yaw: p3,
                height: p4,
            },
            "wiggle_hips" => RobotAction::WiggleHips,
            "heart" => RobotAction::Heart,
            "dance1" => RobotAction::Dance1,
            "dance2" => RobotAction::Dance2,
            "scrape" => RobotAction::Scrape,
            "front_flip" => RobotAction::FrontFlip,
            "front_jump" => RobotAction::FrontJump,
            "front_pounce" => RobotAction::FrontPounce,
            "status" => RobotAction::Status,
            "lidar_on" => RobotAction::LidarOn,
            "lidar_off" => RobotAction::LidarOff,
            "obstacle_avoid_on" => RobotAction::ObstacleAvoidOn,
            "obstacle_avoid_off" => RobotAction::ObstacleAvoidOff,
            "lidar_frame" => RobotAction::LidarFrame,
            _ => return None,
        })
    }

    /// Map a flat `RobotActionWire` (SDK + host ABI minicbor shape) onto the core
    /// allowlist via the shared `from_kind_params`.
    pub fn from_wire(wire: &tentaflow_sdk_spec::RobotActionWire) -> Option<RobotAction> {
        Self::from_kind_params(
            &wire.kind, wire.vx, wire.vy, wire.vyaw, wire.p1, wire.p2, wire.p3, wire.p4,
        )
    }

    /// E-stop-class actions are never blocked by an active e-stop latch and are
    /// never suppressed as "already failed" by the idempotency cache (repeating a
    /// stop is always safe and desirable).
    pub fn is_estop_class(&self) -> bool {
        matches!(self, RobotAction::Estop | RobotAction::Stop)
    }

    /// Read-only (reports state, never moves hardware or changes a latch).
    pub fn is_read_only(&self) -> bool {
        matches!(self, RobotAction::Status | RobotAction::LidarFrame)
    }

    /// Audit-safe label: the action NAME only — never the `Move` velocity values
    /// (codex: no raw movement payloads in logs).
    pub fn audit_label(&self) -> &'static str {
        match self {
            RobotAction::Move { .. } => "Move",
            RobotAction::Stop => "Stop",
            RobotAction::Estop => "Estop",
            RobotAction::ResetEstop => "ResetEstop",
            RobotAction::RecoveryStand => "RecoveryStand",
            RobotAction::StandUp => "StandUp",
            RobotAction::StandDown => "StandDown",
            RobotAction::BalanceStand => "BalanceStand",
            RobotAction::Sit => "Sit",
            RobotAction::Hello => "Hello",
            RobotAction::Stretch => "Stretch",
            RobotAction::Euler { .. } => "Euler",
            RobotAction::BodyHeight { .. } => "BodyHeight",
            RobotAction::FootRaiseHeight { .. } => "FootRaiseHeight",
            RobotAction::SpeedLevel { .. } => "SpeedLevel",
            RobotAction::Pose { .. } => "Pose",
            RobotAction::WiggleHips => "WiggleHips",
            RobotAction::Heart => "Heart",
            RobotAction::Dance1 => "Dance1",
            RobotAction::Dance2 => "Dance2",
            RobotAction::Scrape => "Scrape",
            RobotAction::FrontFlip => "FrontFlip",
            RobotAction::FrontJump => "FrontJump",
            RobotAction::FrontPounce => "FrontPounce",
            RobotAction::Status => "Status",
            RobotAction::LidarOn => "LidarOn",
            RobotAction::LidarOff => "LidarOff",
            RobotAction::ObstacleAvoidOn => "ObstacleAvoidOn",
            RobotAction::ObstacleAvoidOff => "ObstacleAvoidOff",
            RobotAction::LidarFrame => "LidarFrame",
        }
    }

    /// Minimum permission the receiver must verify for this action. Split so a
    /// `robot.telemetry` grant can never move hardware.
    pub fn required_permission(&self) -> &'static str {
        match self {
            RobotAction::Status | RobotAction::LidarFrame => "robot.telemetry",
            RobotAction::Stop | RobotAction::Estop | RobotAction::ResetEstop => "robot.estop",
            _ => "robot.command",
        }
    }

    /// Return a safety-clamped copy, or `Err(RejectReason::Malformed)` if ANY
    /// numeric param is non-finite (NaN / ±inf). NaN/inf are REJECTED, never
    /// coerced — coercing infinity would let a CBOR/mesh/browser sender command
    /// max tilt/height/speed by sending infinity. Finite-but-out-of-range values
    /// are clamped to the documented safety envelope (clamping in-range motion is
    /// the intended behavior). Non-numeric actions are returned unchanged.
    pub fn sanitized(&self, max_velocity: f64) -> Result<RobotAction, RejectReason> {
        Ok(match self {
            RobotAction::Move { vx, vy, vyaw } => {
                if !vx.is_finite() || !vy.is_finite() || !vyaw.is_finite() {
                    return Err(RejectReason::Malformed);
                }
                let cap = max_velocity.clamp(0.0, MAX_VELOCITY);
                RobotAction::Move {
                    vx: clamp_velocity(*vx, cap),
                    vy: clamp_velocity(*vy, cap),
                    vyaw: clamp_velocity(*vyaw, cap),
                }
            }
            RobotAction::Euler { roll, pitch, yaw } => {
                if !roll.is_finite() || !pitch.is_finite() || !yaw.is_finite() {
                    return Err(RejectReason::Malformed);
                }
                RobotAction::Euler {
                    roll: clamp_range(*roll, -EULER_LIMIT, EULER_LIMIT),
                    pitch: clamp_range(*pitch, -EULER_LIMIT, EULER_LIMIT),
                    yaw: clamp_range(*yaw, -EULER_LIMIT, EULER_LIMIT),
                }
            }
            RobotAction::BodyHeight { height } => {
                if !height.is_finite() {
                    return Err(RejectReason::Malformed);
                }
                RobotAction::BodyHeight {
                    height: clamp_range(*height, BODY_HEIGHT_MIN, BODY_HEIGHT_MAX),
                }
            }
            RobotAction::FootRaiseHeight { height } => {
                if !height.is_finite() {
                    return Err(RejectReason::Malformed);
                }
                RobotAction::FootRaiseHeight {
                    height: clamp_range(*height, FOOT_RAISE_MIN, FOOT_RAISE_MAX),
                }
            }
            RobotAction::SpeedLevel { level } => {
                if !level.is_finite() {
                    return Err(RejectReason::Malformed);
                }
                RobotAction::SpeedLevel {
                    // Discrete -1/0/1; round finite then clamp.
                    level: level.round().clamp(-1.0, 1.0),
                }
            }
            RobotAction::Pose {
                roll,
                pitch,
                yaw,
                height,
            } => {
                if !roll.is_finite()
                    || !pitch.is_finite()
                    || !yaw.is_finite()
                    || !height.is_finite()
                {
                    return Err(RejectReason::Malformed);
                }
                RobotAction::Pose {
                    roll: clamp_range(*roll, -EULER_LIMIT, EULER_LIMIT),
                    pitch: clamp_range(*pitch, -EULER_LIMIT, EULER_LIMIT),
                    yaw: clamp_range(*yaw, -EULER_LIMIT, EULER_LIMIT),
                    height: clamp_range(*height, BODY_HEIGHT_MIN, BODY_HEIGHT_MAX),
                }
            }
            other => other.clone(),
        })
    }

    /// The ONLY action→addon bridge. Maps a (sanitized) `RobotAction` to a
    /// concrete go2 addon invocation: either a tool call or a flow block. This
    /// is an explicit allowlist — there is no free-form "call any tool" path, so
    /// a trusted-peer command can never be turned into arbitrary addon execution.
    /// Pure: builds the params shape only, performs no I/O.
    ///
    /// `Move` becomes the `go2.move` flow block with a FlowEnvelope-shaped
    /// `variables` map (each axis a `{kind:"json",data:<f64>}` FlowValue) — the
    /// exact shape the go2 block decoder reads (`block_num` → `variables[key]`).
    /// Every other action is a stateless tool call with empty params.
    pub fn to_go2_call(&self) -> Go2Call {
        let tool = |name: &str| Go2Call::Tool {
            tool: name.to_string(),
            params: json!({}),
        };
        match self {
            RobotAction::Move { vx, vy, vyaw } => Go2Call::Block {
                block_type: "go2.move".to_string(),
                params: json!({
                    "variables": {
                        "vx": { "kind": "json", "data": vx },
                        "vy": { "kind": "json", "data": vy },
                        "vyaw": { "kind": "json", "data": vyaw },
                    }
                }),
            },
            RobotAction::Stop | RobotAction::Estop => tool("go2.estop"),
            RobotAction::ResetEstop => tool("go2.reset_estop"),
            RobotAction::RecoveryStand => tool("go2.action_recovery"),
            RobotAction::StandUp => tool("go2.action_standup"),
            RobotAction::StandDown => tool("go2.action_standdown"),
            RobotAction::BalanceStand => tool("go2.action_balance_stand"),
            RobotAction::Sit => tool("go2.action_sit"),
            RobotAction::Hello => tool("go2.action_hello"),
            RobotAction::Stretch => tool("go2.action_stretch"),
            RobotAction::WiggleHips => tool("go2.action_wiggle_hips"),
            RobotAction::Heart => tool("go2.action_heart"),
            RobotAction::Dance1 => tool("go2.action_dance1"),
            RobotAction::Dance2 => tool("go2.action_dance2"),
            RobotAction::Scrape => tool("go2.action_scrape"),
            RobotAction::FrontFlip => tool("go2.action_front_flip"),
            RobotAction::FrontJump => tool("go2.action_front_jump"),
            RobotAction::FrontPounce => tool("go2.action_front_pounce"),
            RobotAction::Euler { roll, pitch, yaw } => Go2Call::Tool {
                tool: "go2.euler".to_string(),
                params: json!({ "roll": roll, "pitch": pitch, "yaw": yaw }),
            },
            RobotAction::BodyHeight { height } => Go2Call::Tool {
                tool: "go2.body_height".to_string(),
                params: json!({ "height": height }),
            },
            RobotAction::FootRaiseHeight { height } => Go2Call::Tool {
                tool: "go2.foot_raise_height".to_string(),
                params: json!({ "height": height }),
            },
            RobotAction::SpeedLevel { level } => Go2Call::Tool {
                tool: "go2.speed_level".to_string(),
                params: json!({ "level": level }),
            },
            RobotAction::Pose {
                roll,
                pitch,
                yaw,
                height,
            } => Go2Call::Tool {
                tool: "go2.pose".to_string(),
                params: json!({ "roll": roll, "pitch": pitch, "yaw": yaw, "height": height }),
            },
            RobotAction::Status => tool("go2.status"),
            RobotAction::LidarOn => tool("go2.lidar_on"),
            RobotAction::LidarOff => tool("go2.lidar_off"),
            RobotAction::ObstacleAvoidOn => tool("go2.obstacle_avoid_on"),
            RobotAction::ObstacleAvoidOff => tool("go2.obstacle_avoid_off"),
            RobotAction::LidarFrame => tool("go2.lidar_frame"),
        }
    }
}

/// How a `RobotAction` is dispatched into the owning robot addon. Either a
/// stateless tool call (`call_tool`) or a flow block invocation (`invoke_block`,
/// the path that carries the FlowEnvelope `variables` the go2 block decoder reads).
#[derive(Debug, Clone, PartialEq)]
pub enum Go2Call {
    /// Stateless tool call: `addon_manager.call_tool(addon_id, tool, params, user)`.
    Tool {
        tool: String,
        params: serde_json::Value,
    },
    /// Flow block invocation: `addon_manager.invoke_block(addon_id, block_type, params_bytes, ...)`.
    Block {
        block_type: String,
        params: serde_json::Value,
    },
}

/// Clamp a value into `[lo, hi]`. Callers reject non-finite values BEFORE reaching
/// here (`sanitized()` returns `RejectReason::Malformed` for NaN/inf), so this only
/// ever sees finite input in practice; the NaN guards remain purely defensive to
/// avoid a `clamp` panic (min>max) if a NaN bound is ever passed.
pub fn clamp_range(v: f64, lo: f64, hi: f64) -> f64 {
    let lo = if lo.is_nan() { 0.0 } else { lo };
    let hi = if hi.is_nan() { 0.0 } else { hi };
    if v.is_nan() {
        return lo;
    }
    v.clamp(lo, hi)
}

/// Clamp one velocity component to `[-cap, cap]`; NaN → 0.0.
pub fn clamp_velocity(v: f64, cap: f64) -> f64 {
    if v.is_nan() {
        return 0.0;
    }
    // A NaN cap (e.g. from bad config) would make `clamp` panic (min>max), so
    // coerce it to 0 (no motion) before clamping.
    let cap = if cap.is_nan() {
        0.0
    } else {
        cap.clamp(0.0, MAX_VELOCITY)
    };
    v.clamp(-cap, cap)
}

/// A cross-node robot control command. `command_id` is the idempotency token; the
/// receiver must verify trust + permission + timing again — never trust the
/// caller's gate alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RobotControlRequest {
    /// Logical robot id (the owning addon's robot), e.g. "go2".
    pub robot_id: String,
    /// Org the command is issued in — the receiver re-checks `actor_user_id`'s
    /// permission in THIS org (never trusts the caller's gate).
    pub org_id: String,
    /// Unique per logical command — drives duplicate suppression.
    pub command_id: String,
    /// The acting user on the originating node (for authz + audit).
    pub actor_user_id: String,
    pub action: RobotAction,
    /// Wall-clock ms when the originator issued the command.
    pub issued_at_ms: u64,
    /// Wall-clock ms after which the command must NOT be executed.
    pub expires_at_ms: u64,
}

/// Why a command was refused before execution (distinct from an execution error).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RejectReason {
    Expired,
    FutureDated,
    /// Window is inconsistent (expires <= issued).
    Malformed,
    MoveDurationTooLong,
    PermissionDenied,
    EstopActive,
    UntrustedPeer,
    UnknownRobot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RobotControlResponse {
    pub ok: bool,
    /// Present for `Status` (a JSON snapshot) or other result payloads.
    pub result_json: Option<String>,
    /// Set when refused before execution.
    pub rejected: Option<RejectReason>,
    /// Set when execution itself failed.
    pub error: Option<String>,
}

impl RobotControlResponse {
    pub fn ok() -> Self {
        Self {
            ok: true,
            result_json: None,
            rejected: None,
            error: None,
        }
    }
    pub fn ok_with(result_json: String) -> Self {
        Self {
            ok: true,
            result_json: Some(result_json),
            rejected: None,
            error: None,
        }
    }
    pub fn rejected(reason: RejectReason) -> Self {
        Self {
            ok: false,
            result_json: None,
            rejected: Some(reason),
            error: None,
        }
    }
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            result_json: None,
            rejected: None,
            error: Some(error.into()),
        }
    }
}

/// Validate a command's timing against the receiver clock. E-stop-class commands
/// skip expiry (a stop must execute even if it sat in a queue), but a future-dated
/// command is always rejected (clock-skew / replay defense).
pub fn validate_timing(req: &RobotControlRequest, now_ms: u64) -> Result<(), RejectReason> {
    if req.issued_at_ms > now_ms.saturating_add(MAX_CLOCK_SKEW_MS) {
        return Err(RejectReason::FutureDated);
    }
    if req.action.is_estop_class() {
        return Ok(());
    }
    // A non-estop command with an inverted/zero window is bogus (and the
    // saturating math below would otherwise mask it).
    if req.expires_at_ms <= req.issued_at_ms {
        return Err(RejectReason::Malformed);
    }
    if now_ms >= req.expires_at_ms {
        return Err(RejectReason::Expired);
    }
    if let RobotAction::Move { .. } = req.action {
        // Cap BOTH the declared window AND the actual receiver-side remaining
        // live-velocity time: with up to MAX_CLOCK_SKEW_MS future-dating allowed,
        // `expires - issued` alone could still leave the robot moving far longer
        // than MAX_MOVE_DURATION_MS measured from when the receiver executes.
        let declared = req.expires_at_ms - req.issued_at_ms;
        let remaining = req.expires_at_ms - now_ms;
        if declared > MAX_MOVE_DURATION_MS || remaining > MAX_MOVE_DURATION_MS {
            return Err(RejectReason::MoveDurationTooLong);
        }
    }
    Ok(())
}

/// Idempotency key: identical commands from the same actor on the same peer for
/// the same robot collapse to one execution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IdemKey {
    pub from_node_id: String,
    pub actor_user_id: String,
    pub robot_id: String,
    pub command_id: String,
}

impl IdemKey {
    pub fn from_request(from_node_id: &str, req: &RobotControlRequest) -> Self {
        Self {
            from_node_id: from_node_id.to_string(),
            actor_user_id: req.actor_user_id.clone(),
            robot_id: req.robot_id.clone(),
            command_id: req.command_id.clone(),
        }
    }
}

/// TTL cache of processed commands → their response. A duplicate returns the
/// cached response WITHOUT re-executing (so a retried `Move` applies once).
/// E-stop-class commands are intentionally not cached: re-running a stop is safe
/// and must never be suppressed.
#[derive(Default)]
pub struct IdempotencyCache {
    entries: HashMap<IdemKey, (u64, RobotControlResponse)>,
}

impl IdempotencyCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Return a prior response for this command if still within the TTL. E-stop-
    /// class actions NEVER consult the cache — even if a stop reuses a command_id
    /// previously seen for another action, it must still execute.
    pub fn get(
        &self,
        key: &IdemKey,
        action: &RobotAction,
        now_ms: u64,
    ) -> Option<RobotControlResponse> {
        if action.is_estop_class() {
            return None;
        }
        self.entries.get(key).and_then(|(ts, resp)| {
            if now_ms.saturating_sub(*ts) <= IDEMPOTENCY_TTL_MS {
                Some(resp.clone())
            } else {
                None
            }
        })
    }

    /// Record the result of executing a command. E-stop-class actions are not
    /// recorded (never deduplicated).
    pub fn record(
        &mut self,
        key: IdemKey,
        action: &RobotAction,
        resp: RobotControlResponse,
        now_ms: u64,
    ) {
        if action.is_estop_class() {
            return;
        }
        self.entries.insert(key, (now_ms, resp));
    }

    /// Drop entries older than the TTL. Called opportunistically by the receiver.
    pub fn evict_expired(&mut self, now_ms: u64) {
        self.entries
            .retain(|_, (ts, _)| now_ms.saturating_sub(*ts) <= IDEMPOTENCY_TTL_MS);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// What the receiver resolved about the local robot addon for a request: the
/// concrete addon instance id to invoke and its movement safety ceiling (from
/// the manifest `[robot.safety].max_linear_mps`, falling back to `MAX_VELOCITY`).
/// Produced by the I/O side (enumerate installed addons, parse manifests); kept
/// separate so the decision logic in `plan_execution` is pure and unit-testable.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRobotAddon {
    pub addon_id: String,
    pub max_velocity: f64,
}

/// The decided dispatch for a request that passed resolution + authorization:
/// the target addon, the actor to run as, and the safety-clamped `Go2Call`.
#[derive(Debug, Clone, PartialEq)]
pub struct RobotExecutionPlan {
    pub addon_id: String,
    pub actor_user_id: String,
    pub call: Go2Call,
}

/// Pure decision: given a request, the (optionally) resolved local robot addon,
/// and whether the actor is authorized, produce either an execution plan or the
/// reason to reject. Deny-by-default: an unresolved addon → `UnknownRobot`, a
/// failed authz → `PermissionDenied`. Sanitization (velocity clamp to the
/// addon's safety ceiling) happens HERE so the plan the handler runs is already
/// safe. No I/O — the handler does resolution + authz lookups and feeds them in,
/// then just runs the returned plan.
pub fn plan_execution(
    req: &RobotControlRequest,
    resolved: Option<&ResolvedRobotAddon>,
    authorized: bool,
) -> Result<RobotExecutionPlan, RejectReason> {
    let resolved = resolved.ok_or(RejectReason::UnknownRobot)?;
    if !authorized {
        return Err(RejectReason::PermissionDenied);
    }
    let action = req.action.sanitized(resolved.max_velocity)?;
    Ok(RobotExecutionPlan {
        addon_id: resolved.addon_id.clone(),
        actor_user_id: req.actor_user_id.clone(),
        call: action.to_go2_call(),
    })
}

/// Run a decided `RobotExecutionPlan` against the local robot addon and turn the
/// raw addon result into a `RobotControlResponse`. The ONE local-execute
/// implementation shared by the mesh RECEIVER (`command_executor::handle_robot_control`,
/// for forwarded commands) and the SENDER (`robot_dispatch::dispatch_robot_action`,
/// when the owning robot is local) — neither side reimplements the dispatch.
///
/// Synchronous: `call_tool` / `invoke_block` are blocking wasmtime calls, so both
/// callers wrap this in `spawn_blocking`. `read_only` selects whether a successful
/// result is serialized into `result_json` (Status snapshot) or collapsed to a
/// bare `ok()`. Never logs `Move` velocity values.
pub fn execute_robot_call(
    addon_manager: &crate::addon::AddonManager,
    plan: &RobotExecutionPlan,
    read_only: bool,
) -> RobotControlResponse {
    let exec: anyhow::Result<serde_json::Value> = match &plan.call {
        Go2Call::Tool { tool, params } => {
            addon_manager.call_tool(&plan.addon_id, tool, params.clone(), &plan.actor_user_id)
        }
        Go2Call::Block { block_type, params } => match serde_json::to_vec(params) {
            Ok(bytes) => addon_manager
                .invoke_block(
                    &plan.addon_id,
                    block_type,
                    &bytes,
                    Some(plan.actor_user_id.clone()),
                    None,
                    ROBOT_BLOCK_FUEL,
                    None,
                )
                .and_then(|raw| {
                    serde_json::from_slice::<serde_json::Value>(&raw)
                        .map_err(|e| anyhow::anyhow!("decode block result: {e}"))
                }),
            Err(e) => Err(anyhow::anyhow!("encode block params: {e}")),
        },
    };
    match exec {
        Ok(json) => {
            // The wasm block/tool call SUCCEEDS (Ok) even when the addon REFUSES the
            // action in-band (e-stop latched, robot offline, data-channel send failed):
            // the refusal is an `{"error": ...}` in the result body. Without surfacing
            // it, a rejected move is reported as success and the operator sees a ✓ toast
            // while the robot never receives the command.
            if let Some(err) = robot_call_inband_error(&json) {
                return RobotControlResponse::failed(err);
            }
            if read_only {
                match serde_json::to_string(&json) {
                    Ok(s) => RobotControlResponse::ok_with(s),
                    Err(e) => RobotControlResponse::failed(format!("serialize status: {e}")),
                }
            } else {
                RobotControlResponse::ok()
            }
        }
        Err(e) => RobotControlResponse::failed(e.to_string()),
    }
}

/// Extract an addon-reported in-band error from a robot call result. The go2 TOOL
/// path returns the raw result (`{"error": ...}`); the BLOCK path nests its result
/// under `meta.go2`. Returns the error string when the action was refused.
fn robot_call_inband_error(json: &serde_json::Value) -> Option<String> {
    fn err_str(v: &serde_json::Value) -> Option<String> {
        v.get("error")
            .and_then(|e| e.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
    }
    err_str(json).or_else(|| {
        json.get("meta")
            .and_then(|m| m.get("go2"))
            .and_then(err_str)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn move_req(id: &str, vx: f64, issued: u64, expires: u64) -> RobotControlRequest {
        RobotControlRequest {
            robot_id: "go2".into(),
            org_id: "org-1".into(),
            command_id: id.into(),
            actor_user_id: "u1".into(),
            action: RobotAction::Move {
                vx,
                vy: 0.0,
                vyaw: 0.0,
            },
            issued_at_ms: issued,
            expires_at_ms: expires,
        }
    }

    #[test]
    fn from_kind_axes_maps_allowlist_and_rejects_unknown() {
        assert_eq!(
            RobotAction::from_kind_axes("move", 0.3, -0.1, 0.2),
            Some(RobotAction::Move {
                vx: 0.3,
                vy: -0.1,
                vyaw: 0.2
            })
        );
        let cases = [
            ("stop", RobotAction::Stop),
            ("estop", RobotAction::Estop),
            ("reset_estop", RobotAction::ResetEstop),
            ("recovery_stand", RobotAction::RecoveryStand),
            ("stand_up", RobotAction::StandUp),
            ("stand_down", RobotAction::StandDown),
            ("balance_stand", RobotAction::BalanceStand),
            ("sit", RobotAction::Sit),
            ("hello", RobotAction::Hello),
            ("stretch", RobotAction::Stretch),
            ("wiggle_hips", RobotAction::WiggleHips),
            ("heart", RobotAction::Heart),
            ("dance1", RobotAction::Dance1),
            ("dance2", RobotAction::Dance2),
            ("scrape", RobotAction::Scrape),
            ("front_flip", RobotAction::FrontFlip),
            ("front_jump", RobotAction::FrontJump),
            ("front_pounce", RobotAction::FrontPounce),
            ("status", RobotAction::Status),
            ("lidar_on", RobotAction::LidarOn),
            ("lidar_off", RobotAction::LidarOff),
            ("obstacle_avoid_on", RobotAction::ObstacleAvoidOn),
            ("obstacle_avoid_off", RobotAction::ObstacleAvoidOff),
            ("lidar_frame", RobotAction::LidarFrame),
        ];
        for (kind, want) in cases {
            assert_eq!(RobotAction::from_kind_axes(kind, 0.0, 0.0, 0.0), Some(want));
        }
        // Closed allowlist: an unknown kind is refused.
        assert_eq!(RobotAction::from_kind_axes("explode", 0.0, 0.0, 0.0), None);
        assert_eq!(RobotAction::from_kind_axes("", 0.0, 0.0, 0.0), None);
    }

    #[test]
    fn from_kind_params_maps_parametered_actions() {
        assert_eq!(
            RobotAction::from_kind_params("euler", 0.0, 0.0, 0.0, 0.1, -0.2, 0.3, 0.0),
            Some(RobotAction::Euler {
                roll: 0.1,
                pitch: -0.2,
                yaw: 0.3
            })
        );
        assert_eq!(
            RobotAction::from_kind_params("body_height", 0.0, 0.0, 0.0, -0.05, 0.0, 0.0, 0.0),
            Some(RobotAction::BodyHeight { height: -0.05 })
        );
        assert_eq!(
            RobotAction::from_kind_params("foot_raise_height", 0.0, 0.0, 0.0, 0.05, 0.0, 0.0, 0.0),
            Some(RobotAction::FootRaiseHeight { height: 0.05 })
        );
        assert_eq!(
            RobotAction::from_kind_params("speed_level", 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0),
            Some(RobotAction::SpeedLevel { level: 1.0 })
        );
        assert_eq!(
            RobotAction::from_kind_params("pose", 0.0, 0.0, 0.0, 0.1, 0.2, 0.3, -0.05),
            Some(RobotAction::Pose {
                roll: 0.1,
                pitch: 0.2,
                yaw: 0.3,
                height: -0.05
            })
        );
    }

    #[test]
    fn sanitized_clamps_euler_body_foot_speed_and_rejects_nan() {
        // Finite-but-out-of-range Euler is clamped to ±limit.
        assert_eq!(
            RobotAction::Euler {
                roll: 5.0,
                pitch: -5.0,
                yaw: 0.1
            }
            .sanitized(1.0),
            Ok(RobotAction::Euler {
                roll: EULER_LIMIT,
                pitch: -EULER_LIMIT,
                yaw: 0.1
            })
        );
        // A NaN axis is REJECTED (never coerced to a bound).
        assert_eq!(
            RobotAction::Euler {
                roll: 5.0,
                pitch: -5.0,
                yaw: f64::NAN
            }
            .sanitized(1.0),
            Err(RejectReason::Malformed)
        );
        // +inf axis is REJECTED.
        assert_eq!(
            RobotAction::Euler {
                roll: f64::INFINITY,
                pitch: 0.0,
                yaw: 0.0
            }
            .sanitized(1.0),
            Err(RejectReason::Malformed)
        );
        // Body height clamped to range; out-of-range below clamps to min.
        assert_eq!(
            RobotAction::BodyHeight { height: -1.0 }.sanitized(1.0),
            Ok(RobotAction::BodyHeight {
                height: BODY_HEIGHT_MIN
            })
        );
        assert_eq!(
            RobotAction::BodyHeight { height: 1.0 }.sanitized(1.0),
            Ok(RobotAction::BodyHeight {
                height: BODY_HEIGHT_MAX
            })
        );
        // NaN / -inf body height is REJECTED.
        assert_eq!(
            RobotAction::BodyHeight { height: f64::NAN }.sanitized(1.0),
            Err(RejectReason::Malformed)
        );
        assert_eq!(
            RobotAction::BodyHeight {
                height: f64::NEG_INFINITY
            }
            .sanitized(1.0),
            Err(RejectReason::Malformed)
        );
        // Foot raise clamped; non-finite rejected.
        assert_eq!(
            RobotAction::FootRaiseHeight { height: 9.0 }.sanitized(1.0),
            Ok(RobotAction::FootRaiseHeight {
                height: FOOT_RAISE_MAX
            })
        );
        assert_eq!(
            RobotAction::FootRaiseHeight {
                height: f64::INFINITY
            }
            .sanitized(1.0),
            Err(RejectReason::Malformed)
        );
        // Speed level rounded + clamped to discrete -1/0/1.
        assert_eq!(
            RobotAction::SpeedLevel { level: 7.0 }.sanitized(1.0),
            Ok(RobotAction::SpeedLevel { level: 1.0 })
        );
        assert_eq!(
            RobotAction::SpeedLevel { level: 0.4 }.sanitized(1.0),
            Ok(RobotAction::SpeedLevel { level: 0.0 })
        );
        // NaN / inf speed level is REJECTED (not coerced to 0/1).
        assert_eq!(
            RobotAction::SpeedLevel { level: f64::NAN }.sanitized(1.0),
            Err(RejectReason::Malformed)
        );
        assert_eq!(
            RobotAction::SpeedLevel {
                level: f64::INFINITY
            }
            .sanitized(1.0),
            Err(RejectReason::Malformed)
        );
        // Pose clamps both orientation and height when all finite.
        assert_eq!(
            RobotAction::Pose {
                roll: 5.0,
                pitch: 0.0,
                yaw: 0.0,
                height: -1.0
            }
            .sanitized(1.0),
            Ok(RobotAction::Pose {
                roll: EULER_LIMIT,
                pitch: 0.0,
                yaw: 0.0,
                height: BODY_HEIGHT_MIN
            })
        );
        // Any non-finite Pose param REJECTS (no partial sanitization).
        assert_eq!(
            RobotAction::Pose {
                roll: 0.0,
                pitch: 0.0,
                yaw: 0.0,
                height: f64::NAN
            }
            .sanitized(1.0),
            Err(RejectReason::Malformed)
        );
    }

    #[test]
    fn new_motion_actions_require_robot_command_only_status_read_only() {
        for a in [
            RobotAction::BalanceStand,
            RobotAction::Euler {
                roll: 0.0,
                pitch: 0.0,
                yaw: 0.0,
            },
            RobotAction::BodyHeight { height: 0.0 },
            RobotAction::FootRaiseHeight { height: 0.0 },
            RobotAction::SpeedLevel { level: 0.0 },
            RobotAction::Pose {
                roll: 0.0,
                pitch: 0.0,
                yaw: 0.0,
                height: 0.0,
            },
            RobotAction::WiggleHips,
            RobotAction::Heart,
            RobotAction::Dance1,
            RobotAction::Dance2,
            RobotAction::Scrape,
            RobotAction::FrontFlip,
            RobotAction::FrontJump,
            RobotAction::FrontPounce,
        ] {
            assert_eq!(a.required_permission(), "robot.command", "{a:?}");
            assert!(!a.is_read_only(), "{a:?}");
            assert!(!a.is_estop_class(), "{a:?}");
        }
        assert!(RobotAction::Status.is_read_only());
    }

    #[test]
    fn parametered_actions_map_to_param_tools() {
        match (RobotAction::Euler {
            roll: 0.1,
            pitch: -0.2,
            yaw: 0.3,
        })
        .to_go2_call()
        {
            Go2Call::Tool { tool, params } => {
                assert_eq!(tool, "go2.euler");
                assert_eq!(params.get("roll").and_then(|v| v.as_f64()), Some(0.1));
                assert_eq!(params.get("pitch").and_then(|v| v.as_f64()), Some(-0.2));
                assert_eq!(params.get("yaw").and_then(|v| v.as_f64()), Some(0.3));
            }
            other => panic!("expected Tool, got {other:?}"),
        }
        match (RobotAction::SpeedLevel { level: 1.0 }).to_go2_call() {
            Go2Call::Tool { tool, params } => {
                assert_eq!(tool, "go2.speed_level");
                assert_eq!(params.get("level").and_then(|v| v.as_f64()), Some(1.0));
            }
            other => panic!("expected Tool, got {other:?}"),
        }
    }

    #[test]
    fn clamp_velocity_caps_and_handles_nan() {
        assert_eq!(clamp_velocity(2.0, 1.0), 1.0);
        assert_eq!(clamp_velocity(-2.0, 1.0), -1.0);
        assert_eq!(clamp_velocity(0.3, 1.0), 0.3);
        assert_eq!(clamp_velocity(0.9, 0.5), 0.5); // safety cap below protocol max
        assert_eq!(clamp_velocity(f64::NAN, 1.0), 0.0);
    }

    #[test]
    fn sanitized_clamps_move_to_safety_cap() {
        let a = RobotAction::Move {
            vx: 5.0,
            vy: -5.0,
            vyaw: 0.7,
        }
        .sanitized(0.5);
        assert_eq!(
            a,
            Ok(RobotAction::Move {
                vx: 0.5,
                vy: -0.5,
                vyaw: 0.5
            })
        );
        // non-move unchanged
        assert_eq!(RobotAction::Sit.sanitized(0.5), Ok(RobotAction::Sit));
    }

    #[test]
    fn permission_split_telemetry_cannot_move() {
        assert_eq!(RobotAction::Status.required_permission(), "robot.telemetry");
        assert_eq!(RobotAction::Estop.required_permission(), "robot.estop");
        assert_eq!(RobotAction::Stop.required_permission(), "robot.estop");
        assert_eq!(
            RobotAction::Move {
                vx: 0.0,
                vy: 0.0,
                vyaw: 0.0
            }
            .required_permission(),
            "robot.command"
        );
        assert_eq!(RobotAction::Hello.required_permission(), "robot.command");
        // LiDAR enable/disable is an actuator toggle → robot.command; the read-only
        // frame fetch is telemetry-class and can never move hardware.
        assert_eq!(RobotAction::LidarOn.required_permission(), "robot.command");
        assert_eq!(RobotAction::LidarOff.required_permission(), "robot.command");
        assert_eq!(
            RobotAction::LidarFrame.required_permission(),
            "robot.telemetry"
        );
        assert!(RobotAction::LidarFrame.is_read_only());
        assert!(!RobotAction::LidarOn.is_read_only());
        // Obstacle-avoidance toggle is an actuator toggle → robot.command, never read-only.
        assert_eq!(
            RobotAction::ObstacleAvoidOn.required_permission(),
            "robot.command"
        );
        assert_eq!(
            RobotAction::ObstacleAvoidOff.required_permission(),
            "robot.command"
        );
        assert!(!RobotAction::ObstacleAvoidOn.is_read_only());
    }

    #[test]
    fn lidar_actions_map_to_go2_tools() {
        let tool_of = |a: RobotAction| match a.to_go2_call() {
            Go2Call::Tool { tool, .. } => tool,
            other => panic!("expected Tool, got {other:?}"),
        };
        assert_eq!(tool_of(RobotAction::LidarOn), "go2.lidar_on");
        assert_eq!(tool_of(RobotAction::LidarOff), "go2.lidar_off");
        assert_eq!(tool_of(RobotAction::LidarFrame), "go2.lidar_frame");
        assert_eq!(
            tool_of(RobotAction::ObstacleAvoidOn),
            "go2.obstacle_avoid_on"
        );
        assert_eq!(
            tool_of(RobotAction::ObstacleAvoidOff),
            "go2.obstacle_avoid_off"
        );
    }

    #[test]
    fn expired_move_rejected_estop_always_allowed() {
        let now = 10_000;
        // Expired move.
        let r = move_req("m1", 0.3, 8_000, 9_000);
        assert_eq!(validate_timing(&r, now), Err(RejectReason::Expired));
        // Valid move within window.
        let r2 = move_req("m2", 0.3, 9_500, 11_000);
        assert!(validate_timing(&r2, now).is_ok());
        // Estop is allowed even with an "expired" window.
        let mut e = move_req("e1", 0.0, 1_000, 2_000);
        e.action = RobotAction::Estop;
        assert!(validate_timing(&e, now).is_ok());
    }

    #[test]
    fn future_dated_command_rejected() {
        let now = 10_000;
        let r = move_req("f1", 0.3, now + MAX_CLOCK_SKEW_MS + 1, now + 20_000);
        assert_eq!(validate_timing(&r, now), Err(RejectReason::FutureDated));
        // even estop is rejected if absurdly future-dated (replay defense)
        let mut e = move_req("f2", 0.0, now + MAX_CLOCK_SKEW_MS + 1, now + 1);
        e.action = RobotAction::Estop;
        assert_eq!(validate_timing(&e, now), Err(RejectReason::FutureDated));
    }

    #[test]
    fn move_duration_too_long_rejected() {
        let now = 1_000;
        let r = move_req("d1", 0.3, 1_000, 1_000 + MAX_MOVE_DURATION_MS + 1);
        assert_eq!(
            validate_timing(&r, now),
            Err(RejectReason::MoveDurationTooLong)
        );
        let ok = move_req("d2", 0.3, 1_000, 1_000 + MAX_MOVE_DURATION_MS);
        assert!(validate_timing(&ok, now).is_ok());
    }

    #[test]
    fn move_receiver_side_remaining_capped() {
        // Future-dated within skew, declared window <= max, but the receiver-side
        // remaining (expires - now) exceeds max → must be rejected (the 7s hole).
        let now = 10_000;
        let issued = now + MAX_CLOCK_SKEW_MS; // 15_000, accepted (== now+skew)
        let expires = issued + MAX_MOVE_DURATION_MS; // declared window == max
        let r = move_req("rr", 0.3, issued, expires);
        // remaining = expires - now = 5000 + 2000 = 7000 > 2000 → reject
        assert_eq!(
            validate_timing(&r, now),
            Err(RejectReason::MoveDurationTooLong)
        );
    }

    #[test]
    fn malformed_window_rejected() {
        let now = 10_000;
        // expires < issued (would saturate to 0 duration and slip through).
        let r = move_req(
            "mw",
            0.3,
            now + MAX_CLOCK_SKEW_MS,
            now + MAX_CLOCK_SKEW_MS - 1,
        );
        assert_eq!(validate_timing(&r, now), Err(RejectReason::Malformed));
    }

    #[test]
    fn nan_max_velocity_does_not_panic() {
        let a = RobotAction::Move {
            vx: 0.5,
            vy: 0.5,
            vyaw: 0.5,
        }
        .sanitized(f64::NAN);
        // NaN cap coerced to 0 → no motion (action params are finite, so accepted).
        assert_eq!(
            a,
            Ok(RobotAction::Move {
                vx: 0.0,
                vy: 0.0,
                vyaw: 0.0
            })
        );
    }

    #[test]
    fn idempotency_dedups_move_but_never_estop() {
        let mut cache = IdempotencyCache::new();
        let req = move_req("m1", 0.3, 0, 1_000);
        let key = IdemKey::from_request("nodeB", &req);
        assert!(cache.get(&key, &req.action, 100).is_none());
        cache.record(key.clone(), &req.action, RobotControlResponse::ok(), 100);
        // duplicate within TTL returns cached response
        assert_eq!(
            cache.get(&key, &req.action, 200),
            Some(RobotControlResponse::ok())
        );
        // estop is never recorded → never deduped
        let mut estop = move_req("e1", 0.0, 0, 1_000);
        estop.action = RobotAction::Estop;
        let ekey = IdemKey::from_request("nodeB", &estop);
        cache.record(ekey.clone(), &estop.action, RobotControlResponse::ok(), 100);
        assert!(cache.get(&ekey, &estop.action, 200).is_none());
    }

    #[test]
    fn estop_never_served_from_cache_even_on_id_collision() {
        // A Move recorded under a command_id; a later Estop reusing that exact key
        // must NOT receive the cached Move response — it must execute.
        let mut cache = IdempotencyCache::new();
        let mv = move_req("shared-id", 0.3, 0, 1_000);
        let key = IdemKey::from_request("nodeB", &mv);
        cache.record(key.clone(), &mv.action, RobotControlResponse::ok(), 100);
        // Same key, but querying as an Estop action → cache must return None.
        assert!(cache.get(&key, &RobotAction::Estop, 200).is_none());
        assert!(cache.get(&key, &RobotAction::Stop, 200).is_none());
        // sanity: as a Move it's still cached
        assert!(cache.get(&key, &mv.action, 200).is_some());
    }

    #[test]
    fn idempotency_entry_expires_after_ttl() {
        let mut cache = IdempotencyCache::new();
        let req = move_req("m1", 0.3, 0, 1_000);
        let key = IdemKey::from_request("nodeB", &req);
        cache.record(key.clone(), &req.action, RobotControlResponse::ok(), 1_000);
        assert!(cache
            .get(&key, &req.action, 1_000 + IDEMPOTENCY_TTL_MS)
            .is_some());
        assert!(cache
            .get(&key, &req.action, 1_000 + IDEMPOTENCY_TTL_MS + 1)
            .is_none());
        cache.evict_expired(1_000 + IDEMPOTENCY_TTL_MS + 1);
        assert!(cache.is_empty());
    }

    #[test]
    fn request_cbor_roundtrip() {
        let req = RobotControlRequest {
            robot_id: "go2".into(),
            org_id: "org-1".into(),
            command_id: "c1".into(),
            actor_user_id: "u1".into(),
            action: RobotAction::Move {
                vx: 0.3,
                vy: -0.1,
                vyaw: 0.2,
            },
            issued_at_ms: 123,
            expires_at_ms: 456,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&req, &mut buf).expect("encode");
        let back: RobotControlRequest = ciborium::de::from_reader(&buf[..]).expect("decode");
        assert_eq!(req, back);

        let resp = RobotControlResponse::ok_with("{\"battery\":80}".into());
        let mut rbuf = Vec::new();
        ciborium::ser::into_writer(&resp, &mut rbuf).expect("encode resp");
        let rback: RobotControlResponse =
            ciborium::de::from_reader(&rbuf[..]).expect("decode resp");
        assert_eq!(resp, rback);
    }

    #[test]
    fn to_go2_call_move_builds_flow_variables_shape() {
        let call = RobotAction::Move {
            vx: 0.4,
            vy: -0.2,
            vyaw: 0.1,
        }
        .to_go2_call();
        match call {
            Go2Call::Block { block_type, params } => {
                assert_eq!(block_type, "go2.move");
                let vars = params.get("variables").expect("variables");
                // Each axis is a FlowValue {kind:"json", data:<f64>} the go2
                // block_num decoder reads.
                for (key, want) in [("vx", 0.4), ("vy", -0.2), ("vyaw", 0.1)] {
                    let fv = vars.get(key).expect(key);
                    assert_eq!(fv.get("kind").and_then(|k| k.as_str()), Some("json"));
                    assert_eq!(fv.get("data").and_then(|d| d.as_f64()), Some(want));
                }
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn to_go2_call_actions_map_to_allowlisted_tools() {
        let cases = [
            (RobotAction::Stop, "go2.estop"),
            (RobotAction::Estop, "go2.estop"),
            (RobotAction::ResetEstop, "go2.reset_estop"),
            (RobotAction::RecoveryStand, "go2.action_recovery"),
            (RobotAction::StandUp, "go2.action_standup"),
            (RobotAction::StandDown, "go2.action_standdown"),
            (RobotAction::Sit, "go2.action_sit"),
            (RobotAction::Hello, "go2.action_hello"),
            (RobotAction::Stretch, "go2.action_stretch"),
            (RobotAction::Status, "go2.status"),
        ];
        for (action, want_tool) in cases {
            match action.to_go2_call() {
                Go2Call::Tool { tool, params } => {
                    assert_eq!(tool, want_tool, "action {action:?}");
                    assert_eq!(params, json!({}), "tool params must be empty");
                }
                other => panic!("expected Tool for {action:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn plan_execution_unknown_robot_when_unresolved() {
        let req = move_req("p1", 0.3, 0, 1_000);
        assert_eq!(
            plan_execution(&req, None, true),
            Err(RejectReason::UnknownRobot)
        );
    }

    #[test]
    fn plan_execution_permission_denied_when_unauthorized() {
        let req = move_req("p2", 0.3, 0, 1_000);
        let resolved = ResolvedRobotAddon {
            addon_id: "go2".into(),
            max_velocity: 1.0,
        };
        assert_eq!(
            plan_execution(&req, Some(&resolved), false),
            Err(RejectReason::PermissionDenied)
        );
    }

    #[test]
    fn plan_execution_sanitizes_move_to_addon_safety_cap() {
        // Request asks for vx=5.0; the resolved addon caps at 0.5 → the planned
        // call must already be clamped (the handler never sees the raw value).
        let req = move_req("p3", 5.0, 0, 1_000);
        let resolved = ResolvedRobotAddon {
            addon_id: "go2-1".into(),
            max_velocity: 0.5,
        };
        let plan = plan_execution(&req, Some(&resolved), true).expect("plan");
        assert_eq!(plan.addon_id, "go2-1");
        assert_eq!(plan.actor_user_id, "u1");
        match plan.call {
            Go2Call::Block { block_type, params } => {
                assert_eq!(block_type, "go2.move");
                let vx = params
                    .get("variables")
                    .and_then(|v| v.get("vx"))
                    .and_then(|v| v.get("data"))
                    .and_then(|d| d.as_f64());
                assert_eq!(vx, Some(0.5));
            }
            other => panic!("expected Block, got {other:?}"),
        }
    }

    #[test]
    fn plan_execution_estop_maps_to_tool() {
        let mut req = move_req("p4", 0.0, 0, 1_000);
        req.action = RobotAction::Estop;
        let resolved = ResolvedRobotAddon {
            addon_id: "go2".into(),
            max_velocity: 1.0,
        };
        let plan = plan_execution(&req, Some(&resolved), true).expect("plan");
        assert_eq!(
            plan.call,
            Go2Call::Tool {
                tool: "go2.estop".into(),
                params: json!({})
            }
        );
    }
}
