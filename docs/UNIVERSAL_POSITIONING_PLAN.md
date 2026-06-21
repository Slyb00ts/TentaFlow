# Universal Positioning / Localization — Plan ("always know where we are")

## 0. Goal & guiding principle

Produce, for **any device**, in **any environment** (indoor / outdoor / aerial /
ground / underground), from **any subset of available sensors**, a single answer:

> **Global pose** = WGS84 `lat / lon / alt` + orientation (quaternion) + velocity,
> each with an explicit **covariance** and a record of **which sensors contributed**.

Hard requirements:

- **Self-locating** ("kidnapped robot"): boot with no prior and figure out where it
  is — both *in the world* and *inside a building* — automatically.
- **Robust to sensor loss / jamming**: if every wireless signal is jammed, a device
  with only a camera (e.g. a drone in flight) must still localize against **saved
  maps**. No single sensor is load-bearing.
- **Accurate**: cm-level where a metric map exists (LiDAR), sub-meter to meter for
  visual / RF, gracefully coarser when only weak cues remain.
- **Heterogeneous by design**: every device has *completely different* data. The
  fusion core must not know device specifics.

Guiding principle: **one probabilistic state, many heterogeneous constraints.**
Everything else is plumbing.

---

## 1. Core idea — one state, sensors as constraints

- A single estimated **state**: global pose + velocity + IMU biases (+ clock /
  per-sensor extrinsics where needed), with covariance.
- **Every sensor is abstracted to a CONSTRAINT** = a measurement + a noise model
  relating it to the state. The fusion engine consumes only constraints; it never
  sees raw device formats.
- **Per-device addon = adapter**: converts that device's raw stream into *canonical
  measurement messages* (§11). "Different devices, different data" is solved here,
  exactly like a robot driver addon converts its sensor into the canonical LiDAR
  frame.
- **"Any combination" is automatic**: an absent sensor is simply a missing
  constraint. The estimator uses whatever arrives and reports the resulting
  uncertainty. Add a sensor → more constraints → tighter estimate. Lose one →
  wider covariance, never a crash.

---

## 2. Two localization layers (relative + absolute)

| Layer | Sensors | Property | Role |
|------|---------|----------|------|
| **Relative (odometry)** | IMU, visual-inertial (VIO), LiDAR-inertial (LIO), wheel/leg odometry | smooth, high-rate, **drifts** over time/distance | local motion backbone |
| **Absolute (global anchoring)** | GNSS, map-relative reloc (visual/LiDAR), WiFi/BLE fingerprint, UWB/known anchors, cross-view (aerial↔satellite) | low-rate, **drift-free**, ties to WGS84 | kills drift, gives global truth |

Accurate global pose = tight relative odometry (smooth, no gaps) **+ frequent
absolute corrections** (no long-term drift). This is the standard winning pattern
(VIO/LIO + GNSS/map fixes) generalized to all sensors.

---

## 3. Coordinate frames — the transform chain (how it all ties together)

```
 body (device)  ──T_body→map──►  local scene/map frame  ──T_map→ECEF──►  ECEF / WGS84
   (IMU,cam,...)   localization        (per building/area)   georeference     → lat/lon/alt
```

- `T_body→map`: from localization (odometry + map-relative reloc).
- `T_map→ECEF`: the **georeference** of each scene — fixed when the map was built
  (with GNSS, or from surveyed control points / cross-view alignment). Lives with
  the scene in the SPATIAL store (`SPATIAL_3D_PLAN.md` §11).
- Compose → `T_body→ECEF` → GPS. **The output is uniform** (a global pose) no
  matter which path produced it: outdoors GNSS anchors the chain directly; indoors
  / jammed, map-relative reloc provides `T_body→map` and the scene's georeference
  provides `T_map→ECEF`.
- Outdoors with continuous GNSS, the map frame is anchored every fix; indoors it is
  anchored at each reloc and propagated by odometry between fixes.

---

## 4. Sensor → constraint catalog

Each sensor declares: what it constrains, absolute vs relative, rough noise, and
the precondition that makes it valid. The engine reads these from the canonical
message; it does not hardcode device types.

| Sensor | Constrains | Abs/Rel | Rough accuracy | Valid when |
|--------|-----------|---------|----------------|-----------|
| IMU (gyro+accel) | Δpose between times (preintegrated), gravity dir | rel | drifts; gravity gives roll/pitch | always (backbone) |
| Barometer | altitude (relative, weather-biased) | semi-abs | ~1 m short-term | always; recalibrate on GNSS/map |
| Magnetometer | heading (yaw) | abs-ish | good outdoors, poor indoors | uncluttered magnetic env |
| GNSS / RTK | global position (+vel) | abs | m / cm(RTK) | sky view, not jammed |
| WiFi / BLE RSSI | coarse position vs AP DB / fingerprint | abs | 2–10 m | known AP map / fingerprint exists |
| UWB / anchors | precise range/bearing to known points | abs | cm–dm | anchors deployed (optional) |
| Camera — VO/VIO | Δpose, scale (with IMU) | rel | drifts | texture/parallax present |
| Camera — VPR + PnP | 6DoF pose vs **map** | abs | dm–m | map of the place exists |
| Camera — cross-view | global pos vs satellite/orthophoto | abs | m–tens m | aerial / open view + ortho tiles |
| LiDAR — odometry | Δpose | rel | drifts slowly | geometric structure |
| LiDAR — scan-to-map (ICP/NDT) | 6DoF pose vs **map** | abs | cm | metric map of the place exists |
| Wheel / leg odometry | Δpose (planar) | rel | slip-prone | ground contact |
| Cellular / TDOA | very coarse position | abs | tens–hundreds m | last-resort outdoor |

---

## 5. GNSS-denied / jammed → map-relative positioning (the killer requirement)

When all RF is gone, **maps are the source of global truth**. The SPATIAL store's
scenes are **georeferenced** (`T_map→ECEF`), so a pose-in-map becomes a GPS fix.

- **Ground + LiDAR**: scan-to-map registration (ICP / NDT / learned) → 6DoF pose in
  the scene → GPS. cm-level.
- **Drone / device + camera only**: 
  1. **Visual Place Recognition (VPR)** — a learned global image descriptor
     (NetVLAD-style) retrieves candidate keyframes from the scene's map index.
  2. **PnP / 2D-3D match** against the map's keypoints → 6DoF pose → GPS.
  3. **VIO bridges between VPR fixes**, so localization continues *in flight*
     between recognitions; each VPR fix corrects drift.
- **Aerial, no local map**: **cross-view geolocalization** — match the downward /
  oblique camera view to **public satellite / orthophoto tiles** (a learned
  cross-view embedding, or render-and-compare). Gives a global fix without any
  pre-flown local map. Refined by VIO + a barometer/altimeter for altitude.

This is the literal "someone jams everything, the drone localizes from camera +
saved maps" scenario, decomposed into retrieval → geometric verification → fuse.

---

## 6. Self-location (kidnapped / cold start)

On boot with no prior, resolve *which place* before refining *where in it*:

1. **GNSS** → instant global, pick the scene whose georeference contains it.
2. **Global place recognition** over **all known scene maps** — a single VPR /
   LiDAR-descriptor index across scenes returns candidate scene(s) + coarse pose;
   verify geometrically (PnP / registration). This is "sam się odnaleźć": it
   *searches the map database*, indoors or out.
3. **WiFi/BLE fingerprint** → which building/floor (coarse), then hand to VPR.
4. **Cellular** → coarse region (narrows the VPR search).
5. **Dead-reckon** from last-known via IMU until a recognizable landmark appears.

Output is coarse→refined: a wide-covariance fix immediately, tightening as
geometric verification and odometry converge.

---

## 7. Fusion engine design

- **Primary: sliding-window factor graph.** Nodes = poses over a time window;
  factors = IMU preintegration (between poses), GNSS prior, map-match prior,
  WiFi/UWB prior, barometer, loop closures. Incremental optimization (iSAM-style)
  with marginalization of old nodes. Naturally loosely-coupled and heterogeneous;
  factors appear/disappear with sensor availability → graceful degradation is the
  default, not a special case.
- **Lightweight profile: Error-State Kalman Filter (ESKF)** for MCUs / phones —
  same canonical inputs and `GlobalPose` output, smaller compute. The engine picks
  a profile from the device's declared capability.
- **Robustness built in:**
  - Robust kernels (Huber / DCS) + per-factor χ² gating reject outliers.
  - **GNSS spoof/jam detection**: a GNSS fix inconsistent with VIO/LIO + map is
    down-weighted or dropped — map-relative becomes the trusted anchor. No single
    absolute source is ever blindly trusted.
  - Every output carries covariance + a **source bitmask** (what contributed), so
    consumers know how much to trust it.

---

## 8. Cross-device / mesh collaborative localization

TentaFlow's mesh makes positioning a fleet capability, not a per-device silo:

- **Map sharing**: georeferenced scenes + VPR/descriptor indexes replicate over the
  mesh, so a device that never mapped a place can still relocalize there.
- **Relative co-localization**: a device WITH GPS that *observes* a GPS-denied
  device (relative range/bearing, or a shared landmark) injects a constraint that
  propagates the global frame to the denied device.
- **Collaborative mapping**: multiple devices co-build/extend a scene; better maps
  → better relocalization for everyone.

---

## 9. TentaFlow architecture mapping

- **Canonical measurement messages** (fixed binary, same philosophy as the LiDAR
  frame — versioned header + packed body, no JSON on hot paths):
  `ImuSample`, `BaroSample`, `MagSample`, `GnssFix`, `WifiScan`, `RangeSample`,
  `VisualKeyframe` (features + global descriptor), `LidarScan` (reuse
  `SpatialFrame`). Each carries timestamp + noise model.
- **Per-device addon** emits these from raw device data (the adapter / "different
  data" boundary).
- **Core Localization Engine** (a service, like the LiDAR hub): consumes canonical
  measurements → maintains the factor-graph/ESKF state → emits a canonical
  **`GlobalPose`** stream (lat/lon/alt, quaternion, velocity, covariance, source
  bitmask, scene id + `T_map→ECEF`).
- **Reuses the SPATIAL map store**: georeferenced scenes, the VPR/descriptor index,
  and scan/feature registration are the absolute-anchoring providers.
- Output `GlobalPose` is streamed on the same rails as LiDAR (latest-wins, binary).

---

## 10. Phasing (rough; each phase codex-reviewed, real-device verified)

- **P-A — Outdoor baseline**: ESKF fusing IMU + GNSS + barometer (+ mag) →
  `GlobalPose`. Establishes the canonical messages + engine skeleton + output.
- **P-B — Relative odometry**: VIO (camera+IMU) and LIO (lidar+IMU) as relative
  constraints; drift-bounded local trajectory.
- **P-C — Map georeference + map-relative reloc (GNSS-denied ground)**: scene
  `T_map→ECEF`; LiDAR scan-to-map + visual PnP reloc → drift-free indoors/jammed.
- **P-D — Global place recognition + kidnapped**: cross-scene VPR/descriptor index;
  cold-start self-location.
- **P-E — Aerial cross-view**: camera↔satellite/orthophoto geolocalization for
  drones with no local map.
- **P-F — Mesh collaborative**: map sharing + relative co-localization across nodes.
- **P-G — Robustness upgrade**: full sliding-window factor graph, spoof/jam
  detection, robust kernels, marginalization.

---

## 11. Open questions for codex

1. Factor graph vs ESKF as the *primary* (vs profile-per-device) — maintenance and
   accuracy trade-off; which Rust libraries (gtsam-rs? hand-rolled iSAM? `factrs`?).
2. VPR descriptor choice and on-device cost (learned vs classical BoW); how the
   cross-scene index is stored/sharded in the SPATIAL store and synced over mesh.
3. Cross-view aerial geolocalization feasibility on-device (model size, accuracy)
   vs offloading to a heavier node over the mesh.
4. Georeference establishment without GNSS (surveyed control points? alignment of a
   new scene to an existing georeferenced neighbor via overlap?).
5. Time synchronization across heterogeneous sensors/devices for tight fusion
   (per-sensor latency/clock model in the canonical message?).
6. GNSS spoofing detection thresholds and the precise rule for when map-relative
   overrides a plausible-but-wrong GNSS fix.
7. State observability bookkeeping — detect & report when the available sensor mix
   leaves the global pose unobservable (e.g. featureless corridor, no map, no RF),
   instead of emitting an overconfident wrong fix.
