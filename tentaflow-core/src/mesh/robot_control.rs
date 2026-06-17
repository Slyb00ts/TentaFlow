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
    Move { vx: f64, vy: f64, vyaw: f64 },
    /// Soft stop (stop current motion). E-stop-class: always allowed through.
    Stop,
    /// Emergency stop + durable safety latch. E-stop-class: always allowed through.
    Estop,
    /// Clear the e-stop latch.
    ResetEstop,
    RecoveryStand,
    StandUp,
    StandDown,
    Sit,
    Hello,
    Stretch,
    /// Read-only telemetry/status snapshot.
    Status,
}

impl RobotAction {
    /// E-stop-class actions are never blocked by an active e-stop latch and are
    /// never suppressed as "already failed" by the idempotency cache (repeating a
    /// stop is always safe and desirable).
    pub fn is_estop_class(&self) -> bool {
        matches!(self, RobotAction::Estop | RobotAction::Stop)
    }

    /// Read-only (reports state, never moves hardware or changes a latch).
    pub fn is_read_only(&self) -> bool {
        matches!(self, RobotAction::Status)
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
            RobotAction::Sit => "Sit",
            RobotAction::Hello => "Hello",
            RobotAction::Stretch => "Stretch",
            RobotAction::Status => "Status",
        }
    }

    /// Minimum permission the receiver must verify for this action. Split so a
    /// `robot.telemetry` grant can never move hardware.
    pub fn required_permission(&self) -> &'static str {
        match self {
            RobotAction::Status => "robot.telemetry",
            RobotAction::Stop | RobotAction::Estop | RobotAction::ResetEstop => "robot.estop",
            _ => "robot.command",
        }
    }

    /// Return a safety-clamped copy: velocities clamped to `[-max_velocity,
    /// max_velocity]` (and to the protocol ceiling), NaN coerced to 0. Non-move
    /// actions are returned unchanged.
    pub fn sanitized(&self, max_velocity: f64) -> RobotAction {
        match self {
            RobotAction::Move { vx, vy, vyaw } => {
                let cap = max_velocity.clamp(0.0, MAX_VELOCITY);
                RobotAction::Move {
                    vx: clamp_velocity(*vx, cap),
                    vy: clamp_velocity(*vy, cap),
                    vyaw: clamp_velocity(*vyaw, cap),
                }
            }
            other => other.clone(),
        }
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
            RobotAction::Sit => tool("go2.action_sit"),
            RobotAction::Hello => tool("go2.action_hello"),
            RobotAction::Stretch => tool("go2.action_stretch"),
            RobotAction::Status => tool("go2.status"),
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

/// Clamp one velocity component to `[-cap, cap]`; NaN → 0.0.
pub fn clamp_velocity(v: f64, cap: f64) -> f64 {
    if v.is_nan() {
        return 0.0;
    }
    // A NaN cap (e.g. from bad config) would make `clamp` panic (min>max), so
    // coerce it to 0 (no motion) before clamping.
    let cap = if cap.is_nan() { 0.0 } else { cap.clamp(0.0, MAX_VELOCITY) };
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
        Self { ok: true, result_json: None, rejected: None, error: None }
    }
    pub fn ok_with(result_json: String) -> Self {
        Self { ok: true, result_json: Some(result_json), rejected: None, error: None }
    }
    pub fn rejected(reason: RejectReason) -> Self {
        Self { ok: false, result_json: None, rejected: Some(reason), error: None }
    }
    pub fn failed(error: impl Into<String>) -> Self {
        Self { ok: false, result_json: None, rejected: None, error: Some(error.into()) }
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
        Self { entries: HashMap::new() }
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
    pub fn record(&mut self, key: IdemKey, action: &RobotAction, resp: RobotControlResponse, now_ms: u64) {
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
    let action = req.action.sanitized(resolved.max_velocity);
    Ok(RobotExecutionPlan {
        addon_id: resolved.addon_id.clone(),
        actor_user_id: req.actor_user_id.clone(),
        call: action.to_go2_call(),
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
            action: RobotAction::Move { vx, vy: 0.0, vyaw: 0.0 },
            issued_at_ms: issued,
            expires_at_ms: expires,
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
        let a = RobotAction::Move { vx: 5.0, vy: -5.0, vyaw: 0.7 }.sanitized(0.5);
        assert_eq!(a, RobotAction::Move { vx: 0.5, vy: -0.5, vyaw: 0.5 });
        // non-move unchanged
        assert_eq!(RobotAction::Sit.sanitized(0.5), RobotAction::Sit);
    }

    #[test]
    fn permission_split_telemetry_cannot_move() {
        assert_eq!(RobotAction::Status.required_permission(), "robot.telemetry");
        assert_eq!(RobotAction::Estop.required_permission(), "robot.estop");
        assert_eq!(RobotAction::Stop.required_permission(), "robot.estop");
        assert_eq!(
            RobotAction::Move { vx: 0.0, vy: 0.0, vyaw: 0.0 }.required_permission(),
            "robot.command"
        );
        assert_eq!(RobotAction::Hello.required_permission(), "robot.command");
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
        assert_eq!(validate_timing(&r, now), Err(RejectReason::MoveDurationTooLong));
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
        assert_eq!(validate_timing(&r, now), Err(RejectReason::MoveDurationTooLong));
    }

    #[test]
    fn malformed_window_rejected() {
        let now = 10_000;
        // expires < issued (would saturate to 0 duration and slip through).
        let r = move_req("mw", 0.3, now + MAX_CLOCK_SKEW_MS, now + MAX_CLOCK_SKEW_MS - 1);
        assert_eq!(validate_timing(&r, now), Err(RejectReason::Malformed));
    }

    #[test]
    fn nan_max_velocity_does_not_panic() {
        let a = RobotAction::Move { vx: 0.5, vy: 0.5, vyaw: 0.5 }.sanitized(f64::NAN);
        // NaN cap coerced to 0 → no motion.
        assert_eq!(a, RobotAction::Move { vx: 0.0, vy: 0.0, vyaw: 0.0 });
    }

    #[test]
    fn idempotency_dedups_move_but_never_estop() {
        let mut cache = IdempotencyCache::new();
        let req = move_req("m1", 0.3, 0, 1_000);
        let key = IdemKey::from_request("nodeB", &req);
        assert!(cache.get(&key, &req.action, 100).is_none());
        cache.record(key.clone(), &req.action, RobotControlResponse::ok(), 100);
        // duplicate within TTL returns cached response
        assert_eq!(cache.get(&key, &req.action, 200), Some(RobotControlResponse::ok()));
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
        assert!(cache.get(&key, &req.action, 1_000 + IDEMPOTENCY_TTL_MS).is_some());
        assert!(cache.get(&key, &req.action, 1_000 + IDEMPOTENCY_TTL_MS + 1).is_none());
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
            action: RobotAction::Move { vx: 0.3, vy: -0.1, vyaw: 0.2 },
            issued_at_ms: 123,
            expires_at_ms: 456,
        };
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&req, &mut buf).expect("encode");
        let back: RobotControlRequest =
            ciborium::de::from_reader(&buf[..]).expect("decode");
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
        let call = RobotAction::Move { vx: 0.4, vy: -0.2, vyaw: 0.1 }.to_go2_call();
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
        let resolved = ResolvedRobotAddon { addon_id: "go2".into(), max_velocity: 1.0 };
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
        let resolved = ResolvedRobotAddon { addon_id: "go2-1".into(), max_velocity: 0.5 };
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
        let resolved = ResolvedRobotAddon { addon_id: "go2".into(), max_velocity: 1.0 };
        let plan = plan_execution(&req, Some(&resolved), true).expect("plan");
        assert_eq!(
            plan.call,
            Go2Call::Tool { tool: "go2.estop".into(), params: json!({}) }
        );
    }
}
