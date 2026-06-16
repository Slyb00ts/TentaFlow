# Universal robot model for TentaFlow

Goal: a uniform way to describe, register, and control *any* robot from TentaFlow
(Flow Builder, a dedicated **Robots** app, and optionally TentaVision), regardless
of vendor/kind. Each robot is driven by a **control addon**; Core provides only a
generic transport channel (e.g. `webrtc.*`). All robot-specific logic (signaling,
crypto, command IDs, topics, config) lives in the addon.

## 1. The `[robot]` manifest descriptor (the attribute)

A control addon declares a `[robot]` block in its `manifest.toml`. Its presence
is the marker "this addon controls a robot". The Robots app discovers every
installed addon with this block and offers it when registering a new robot.

Fields (cross-robot, generic — a drone/arm/rover declares the same shape):

- `controls_robot` (bool) — explicit marker.
- `kind` — quadruped | drone | arm | rover | humanoid | …
- `vendor`, `model`, `variant`, `display_name`, `icon`.
- `transports` — how the controller reaches the robot (e.g. `webrtc-lan`).
- `[[robot.connection_param]]` — typed fields the Robots app collects on
  registration (`key`, `label`, `type`, `required`, `placeholder`). Different
  robots need different params (IP, serial, credentials, cloud token, …).
- `[robot.capabilities]` — `locomotion`, `poses`, `actions[]`, `estop`,
  `telemetry[]`, `camera`, `lidar`, `audio`. Lets the app/Flow Builder render
  controls, status tiles and sensor previews uniformly.
- `[robot.safety]` — movement envelope the controller enforces
  (`max_linear_mps`, `max_yaw_rps`, `require_estop_clear`).

Reference instance: `tentaflow-core/addons-pro/go2/manifest.toml`.

## 2. The Robots app (future, dedicated application)

Registers and operates all robots in one place:

- **Register a robot**: pick which control addon to use (from addons with a
  `[robot]` block) → fill the addon-declared `connection_param` form (IP, …) →
  store a robot instance (addon_id + params + display name + org).
- **Status**: online/offline + degraded/busy/e-stop (reported by the addon at
  runtime via events/telemetry, NOT the manifest).
- **Operate**: enter control (locomotion/poses/actions), live camera preview,
  lidar/pointcloud view, telemetry tiles. Movement gated by the safety envelope
  and an explicit e-stop.

Runtime registry (per registered robot): `robot_id`, `addon_id`, connection
params, `display_name`, `org_id`, plus live status. (Lives in the addon's /
Core's storage once the app is built — not yet implemented.)

## 3. Relationship to TentaVision (undecided — kept flexible)

A robot's camera is a normal camera source (registered via `camera.register_backed`
off the addon's WebRTC media track), so TentaVision can run flows against it like
any camera. Open question (decide in practice): whether robot camera/lidar live
fully inside the Robots app, fully inside TentaVision, or split (TentaVision = CV
preview + flows; Robots app = control + lidar + telemetry). The capability
descriptor supports either — TentaVision consumes `camera`, the Robots app
consumes the full capability set.

## 4. Status

Descriptor designed and locked (go2 manifest carries it). Host-side parsing of
`[robot]` and the Robots app itself are NOT built yet — this is a forward design
so addons can declare capabilities consistently from day one. The generic
`webrtc.*` channel (Chunk 1) and the go2 addon code (Chunk 4) come first.
