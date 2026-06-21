# Unified Spatial + Positioning — ONE system (SLAM), correct · accurate · fast

Synthesis that fuses `SPATIAL_3D_PLAN.md` (mapping) and
`UNIVERSAL_POSITIONING_PLAN.md` (localization) into a single design. They are not
two layers — they are **one SLAM loop**: localization needs the map, mapping needs
the pose. This doc defines the shared state, the data flow, the real-time split, the
algorithm/crate decisions, the correctness guarantees, and the mesh model. Priority
order, always: **(1) correct (never a confidently-wrong pose, never a corrupt map),
(2) accurate, (3) fast.**

---

## 1. The one idea that makes it correct AND fast: frozen submaps

The single most important decision. **Geometry is frozen inside submaps; only submap
POSES move.**

- A **submap** is a small, locally-consistent chunk of map (e.g. a few seconds /
  a few metres of integration — a block-hashed TSDF / voxel set, plus the keyframes
  and descriptors that built it), expressed in the submap's OWN local frame. Once
  "sealed", its internal geometry is **immutable**.
- The **global state is a pose graph**: nodes = submap poses `T_submap→scene` (SE(3))
  + live keyframe poses; edges = constraints (odometry, loop closures, GNSS,
  inter-submap registration, georeference). Optimization moves NODE poses only.
- Therefore loop closure / relocalization / georeference / multi-device merge
  **reposition submaps rigidly** — they NEVER re-integrate or rewrite voxels. This
  is what simultaneously gives us:
  - **correctness**: a wrong/late constraint can be removed and the graph re-solved;
    committed geometry is never damaged (no irreversible re-fusion).
  - **speed**: global re-optimization is cheap (it touches N submap poses, not
    millions of voxels); the renderer just re-places submaps by their new pose.
  - **mesh-mergeability**: a submap is a self-contained, content-addressed unit of
    replication (§6).

This is the fix for the SLAM chicken-and-egg and for "the slow path rewrites the map
under the fast path" — see §3.

---

## 2. Single shared data model (the source of truth)

One owner: the **Scene** (per building/area, §SPATIAL-11). A Scene owns:

```
Scene {
  id, georef: Option<SE3_to_ECEF + covariance>,   // null until georeferenced
  pose_graph: {
     nodes:  submap_id -> { pose: SE3, covariance },
     kf_nodes: keyframe_id -> { pose: SE3 },        // sliding window only
     edges:  Constraint[]                            // see below
  },
  submaps: submap_id -> Submap { local geometry (frozen), keyframes, descriptors, hash },
  vpr_index: ANN over keyframe global descriptors,   // place recognition
}
Constraint = Odometry | LoopClosure | Gnss | Georef | InterSubmap | Anchor
           { from, to, measurement: SE3|position, information: Mat, source, status }
   status ∈ { Candidate, Confirmed, Rejected }       // gating, §5
```

Everything else (the canonical `SpatialFrame` on the wire, the `GlobalPose` output)
is derived from this. There is exactly one pose graph per scene; the device that is
actively mapping a scene is its **leader** for live edits, but every node holds a
replica and re-derives poses deterministically (§6) — no central server.

**Output `GlobalPose`** = `T_body→submap (tracking) ∘ T_submap→scene (graph) ∘
T_scene→ECEF (georef)` → WGS84 + covariance (composed, honestly inflated when any
link is weak/missing).

---

## 3. Real-time decomposition (fast path vs slow path)

The fast path must never block on the slow path, and must stay correct while the slow
path rewrites poses underneath it. Submaps make this safe: **the fast path tracks
against the current ACTIVE submap in its LOCAL frame** — a quantity the global
optimizer never touches.

**FAST (every frame, on-device, hard real-time):**
| stage | does | budget (typical) |
|------|------|------|
| IMU propagation | integrate gyro/accel → predicted pose | < 0.5 ms |
| frame→active-submap tracking | ICP (lidar) / sparse VO (camera) vs the active submap | 3–15 ms |
| publish pose | compose with latest submap pose snapshot → `GlobalPose` | < 0.5 ms |

Output rate = sensor rate. The pose is always `local-tracking ∘ submap_pose`, where
`submap_pose` is read lock-free from the latest optimized snapshot. When the slow
path updates submap poses, the fast pose "teleports" by the correction — applied as a
smoothed pose update so the live trajectory stays continuous.

**SLOW (background, lower rate, can be on a heavier mesh node):**
- seal active submap → start new one (on travel/time/feature thresholds);
- integrate frozen geometry (TSDF) for the sealed submap;
- VPR indexing of new keyframes;
- loop-closure detection (VPR → geometric verify) → Candidate edges;
- **pose-graph optimization** (incremental) over Confirmed edges → new submap poses;
- georeference updates (GNSS / control points / cross-view);
- mesh sync of submaps + constraints.

Boundary rule: the fast path consumes an immutable snapshot `{active submap geometry,
all submap poses}` and produces `{new keyframe, new odometry edge, raw frame}`. The
slow path consumes those and produces `{new submap poses, new submaps, new edges}`.
One-directional hand-off each way → no shared mutable state, no locks on the hot path.

---

## 4. Algorithm + Rust-crate decisions (with risk)

| Component | Choice | Rust reality | Risk |
|-----------|--------|--------------|------|
| Linear algebra / SE(3) | `nalgebra` (+ manifold helpers) | solid | low |
| IMU preintegration | hand-roll (Forster et al.) on nalgebra | ~300 lines | low |
| **LiDAR odometry/tracking** | point-to-plane ICP, KISS-ICP-style (voxel downsample + adaptive threshold) | hand-roll on nalgebra + a kd-tree (`kiddo`) | **low** — the tractable path; ship FIRST |
| TSDF / voxel store | block-hashed sparse, hand-roll | per SPATIAL plan | low |
| **Pose-graph / factor backend** | sliding-window + incremental pose-graph (Gauss-Newton / LM, sparse) | `factrs` (pure-Rust factor graphs) as primary; hand-rolled GN on `nalgebra`+`nalgebra-sparse`/`faer` as fallback; **gtsam via FFI** as escalation for full iSAM2 | **medium** — validate `factrs` scale/perf early |
| Scan registration (loop/reloc, lidar) | ICP/GICP with RANSAC pre-align | hand-roll | low |
| **VPR (place recognition)** | learned global descriptor (NetVLAD/CosPlace) via ONNX | **`ort`/tract already in repo** for inference + `hnsw_rs`/`instant-distance` ANN | medium — pick a small model; cross-platform |
| Visual feature front-end (VO) | ORB/AKAZE + IMU (VIO) | `akaze`/`imageproc` exist but **real-time pure-Rust VIO is research-grade** | **HIGH** — biggest risk; defer / consider FFI (e.g. OpenVINS/Basalt) |
| PnP (visual reloc) | P3P + RANSAC + nonlinear refine | hand-roll on nalgebra | medium |
| Georeference align | Umeyama / SE(3) fit from GNSS or control points | `nalgebra` | low |
| Aerial cross-view (cam↔satellite) | learned cross-view embedding via ONNX | `ort` | HIGH — later phase, may offload to a heavy node |

**Locked:** LiDAR-inertial path is the spine and ships first (we already have live
Go2 lidar). Backend = `factrs`-first, gtsam-FFI as the documented escape. Visual
VIO/VPR is explicitly the high-risk research-grade part and is phased AFTER the lidar
loop proves the architecture.

---

## 5. Correctness guarantees (the "bezbłędnie" contract)

**(a) Never fuse a wrong loop closure / relocalization.** Every absolute or loop
constraint is born `Candidate` and must pass ALL of:
1. geometric verification (registration/PnP) inlier ratio ≥ threshold,
2. χ² consistency vs the current graph estimate (Mahalanobis gate),
3. **a second independent confirmation** — a temporally/viewpoint-distinct match, or
   agreement with a different modality — before promotion to `Confirmed`.
Only `Confirmed` edges enter optimization. Robust kernels (Huber/DCS/switchable
constraints) on top, so a survivor that is still wrong is down-weighted, not trusted.
Because geometry is frozen (§1), a constraint later found bad is simply removed and
the graph re-solved — zero geometry damage.

**(b) Never emit a pose more confident than the evidence.** The estimate carries a
covariance; the output composes covariances along `body→submap→scene→ECEF`. An
**observability check** runs each frame: if the live sensor set leaves global pose
unobservable (no map overlap + no GNSS + degenerate geometry), output state =
`Degraded`/`Lost` with inflated covariance — never a crisp wrong number. Absolute
sources cross-check each other; **a single source (incl. GNSS) can never dominate**,
so a spoofed/jammed GNSS inconsistent with map+IMU is gated out.

**(c) Never corrupt a shared map under concurrent writes.** Submaps are
content-addressed, immutable once sealed, append-only. Constraints are append-only
facts. Concurrent device writes add *different* submaps/edges; they never mutate the
same bytes. The pose graph is solved deterministically from the merged constraint set
(§6), so "conflict" reduces to "more constraints" → re-solve, which is convergent.

---

## 6. Mesh consistency (no central server) — reuse the sync ledger

TentaFlow already has a per-node hash-chain sync ledger with convergent
materialization. Map it directly:

- **Submap = replication unit**: `submap_id = (origin_node, seq)`, content-hashed,
  immutable → trivially CRDT-like (add-only set of immutable objects). Replicates as
  ledger ops; the hash dedupes.
- **Constraints = add-only facts** in the ledger (each is an immutable observation
  with provenance + information matrix). The set of constraints is a grow-only set.
- **Pose graph is DERIVED, not synced**: the optimized submap poses are a pure
  deterministic function of the merged (submaps, constraints) set + a fixed gauge
  (anchor the georeferenced submap, else the lowest-id submap at identity).
  **Determinism here is the correctness/failover backstop, NOT a mandate that every
  node optimize.** In practice ONE elected owner per scene runs the optimization and
  publishes the resulting poses as facts; everyone else consumes them cheaply. Because
  the input is a replicated deterministic set, any node can recompute the identical
  result, so owner failover needs no handoff state (the new owner just re-derives).
  See §13 for who computes what.
- **Georeference** is just `Georef` constraints (submap→ECEF) contributed by
  GNSS-equipped nodes; once one node georeferences a scene, the fact replicates and
  every GNSS-denied node inherits a global frame. Multiple/disagreeing georef facts
  are fused (weighted) or gated like any constraint.
- **Relative co-localization**: a node observing another (range/bearing, or a shared
  landmark) emits an inter-device constraint → propagates the global frame to a
  GPS-denied peer through the shared graph.

Gauge/units note: all submaps are metric (same scale) because each is built by a
metric sensor or scale-resolved VIO; cross-device alignment is rigid SE(3), so the
graph stays metrically consistent.

---

## 7. Biggest risk + cheapest de-risk (Phase 0)

**Biggest risk:** the *visual* front-end (real-time VIO + visual relocalization) in
pure Rust is research-grade; betting the architecture on it would stall everything.

**De-risk first (Phase 0), using hardware we already drive live:** prove the WHOLE
loop on **LiDAR only (Go2)** — LIO tracking → submap sealing → frozen TSDF → ICP loop
closure with the §5 gating → pose-graph optimization (`factrs`) → repositioned
submaps → `GlobalPose` out → 2-node mesh merge of submaps+constraints → convergent
poses. This validates the shared state, the fast/slow split, the gating, and the mesh
model on the tractable sensor. Only then add: GNSS/baro georeference (P-A), then the
high-risk visual VIO/VPR (FFI escape allowed), then aerial cross-view.

If `factrs` doesn't scale in Phase 0, the cheap pivot is a hand-rolled sparse
pose-graph GN (poses-only is small) before reaching for gtsam FFI.

---

## 8. Concrete errors / gaps found in the two current docs

1. **Mapping and localization split into separate plans** with no single owner of the
   pose graph → the codependence is unmodeled. Fixed here: one Scene/pose-graph owns
   both; submaps decouple geometry from poses.
2. **No frozen-submap decoupling** in either doc → implies re-fusing geometry on loop
   closure (slow + corruption-prone). This doc makes geometry immutable.
3. **No fast/slow split or latency budget** → "real-time" was asserted, not designed.
   Added in §3.
4. **Bootstrapping unspecified** (first map, no prior, no GNSS): handled — start a
   provisional submap at identity; georeference later when GNSS/control/cross-view
   appears; until then poses are scene-local with honest "no global fix" state.
5. **Mesh convergence of POSES (not just map blocks) undefined** in the sync section →
   §6 makes poses a deterministic function of replicated add-only facts.
6. **Time sync / per-sensor latency** under-specified → each canonical measurement
   must carry sensor-capture timestamp + latency model; fusion is done in a common
   timebase (needed for tight VIO/LIO). (Open item §9.)
7. **Observability / "refuse to answer"** not defined → §5(b) adds the Degraded/Lost
   contract so we never emit a confident wrong pose.
8. **GNSS spoof/jam** mentioned but no rule → §5(b): no single source dominates;
   map-relative overrides an inconsistent GNSS via the χ² gate.

---

## 9. Open questions (need a decision / measurement)

1. `factrs` real-world scale (nodes/edges, incremental update latency) vs hand-rolled
   GN vs gtsam-FFI — benchmark in Phase 0.
2. VPR model + descriptor size that runs cross-platform (phone→server) via `ort`/tract
   at acceptable latency; how the ANN index shards/syncs over the mesh.
3. Submap sealing policy (time/distance/feature-driven) and target submap size — the
   trade between graph size (speed) and re-localization granularity (accuracy).
4. Cross-platform time synchronization across heterogeneous devices for tight fusion
   (hardware timestamps? per-sensor latency calibration?).
5. Georeference without GNSS: bootstrap from surveyed control points, or align a new
   scene to a georeferenced neighbor via overlap — which first?
6. Renderer contract: does the browser receive submap geometry + a live stream of
   submap poses (so it re-places submaps on optimization) — confirm this is the
   `SpatialFrame`/pose-stream split.
7. Gauge convention for deterministic cross-node pose solve (anchor georef submap?
   lowest-id at identity until georeferenced?).

---

## 10. Auto-calibration (observability-driven, mostly NOT ML)

Calibration is NOT a separate mode — the calibration parameters (IMU biases,
camera/lidar↔IMU extrinsics, time offset, monocular scale, gravity) are extra
**state variables in the same graph**. A short "calibration dance" only *accelerates*
their convergence by exciting them; normal operation keeps refining them online.

**Why the dance works — parameters are observable only under excitation:**

| Parameter | Motion that observes it |
|-----------|-------------------------|
| gyro bias | brief **stillness** (gyro at rest = pure bias) |
| gravity → roll/pitch | stillness (accel reads only g) |
| metric scale (mono cam) | **translation with accel/decel** (accel must feel motion) |
| camera↔IMU extrinsics | **rotation + translation in several axes** |
| sensor time offset | fast yaw rotation |
| magnetometer hard/soft-iron | full 3D rotation ("figure-8") |
| wheel odom (radius, baseline) | straight + turns |

Sensible dance: **stillness → slow yaw → 1 m fwd/back → strafe → small figure-8.**
Each element excites a specific row above; nothing is arbitrary.

**Intelligent part = uncertainty-driven, closed-loop ("next-best-motion"):** the
engine watches each parameter's covariance (information-matrix eigenvalues) and (a)
stops when uncertainty is below threshold (adaptive, not fixed-time), (b) if a DoF is
still unobserved, **requests the specific missing motion** ("rotate the other axis",
"step sideways"). All classical (observability), no ML.

**Per-platform, same logic, different actuator:**
- **Robot (Go2):** scripted init routine as a covariance-driven behavior — performs
  yaw/fwd-back/strafe/figure-8, monitors convergence, stops when calibrated; checks
  free space first (lidar/camera) for safety.
- **Phone:** the human is the actuator — AR prompts ("turn slowly", "take a few
  steps") + a coverage bar. ARKit/ARCore already do this at session start; we
  piggyback or replicate.
- **Drone:** brief in-flight pattern (yaw + small axis translations) when safe.

**ML or not?**
- Calibration MATH (extrinsics/bias/scale/time) → **NO ML.** Classical
  self-calibration (parameters as graph variables) is more accurate, provably
  convergent, and yields uncertainty. An ML model emitting "extrinsics" would be
  worse and unverifiable.
- Perception front-ends (metric depth, feature matching, VPR) → **pretrained
  off-the-shelf ONNX**, not trained by us initially.
- Custom training → **optional, later, accuracy boost only**: domain fine-tuning of
  depth/VPR on our own captured environments (the one genuinely worthwhile custom
  model, once we have deployment data); a learned next-best-motion policy is overkill
  vs the observability heuristic.

**Critical caveat:** the dance gives an accurate LOCAL frame (scale, gravity, sensor
alignment) — it is NOT a GPS fix. Global position still needs an absolute anchor
(GNSS, map-relative reloc, or a known start). Dance calibrates *how* we measure; the
anchor says *where* that is on Earth.

---

## 11. Camera ↔ LiDAR unification on POINTS + render modes

Goal: camera and lidar emit the SAME thing — a 3D point cloud — so everything renders
uniformly, with a live per-device 3D view that heat-maps depth.

**Unify at the point level (in the per-device addon):** a camera frame + depth becomes
a colored point cloud by back-projection through the intrinsics `K`:

```
P_xyz = depth · K⁻¹ · [u, v, 1]      // per pixel → 3D point (same XYZ as lidar)
```

So camera → **XYZRGB** points; lidar → **XYZ (+intensity)**. Both flow into the same
canonical `SpatialFrame`, same wire pipeline, same renderer. Core/renderer never know
the source.

**Depth source for the camera (best → fallback):**
1. hardware depth (iPhone LiDAR/ToF, RGB-D) → metric depth for free;
2. stereo → triangulated metric depth;
3. monocular metric depth model (Depth Anything V2 metric / Metric3D v2 / UniDepth)
   via `ort`/tract when there is no depth hardware.

**New canonical layout `XYZRGB`** (+ a compressed/quantized variant) joins `XYZ`,
`XYZI`, `XYZ_I16_PLANAR`. Decoder normalizes everything to `points: Float32Array`
(+ optional `colors`). Camera uses `XYZRGB`; lidar uses `XYZ`/`XYZI`. (Layout stays a
wire concern; see §13 of `SPATIAL_3D_PLAN.md`.)

**Depth heatmap is a SHADER, not data.** Points carry `XYZ`; the header carries the
sensor `origin`. The renderer computes per-point range and colormaps it:

```
range = |P - origin|     near → 🔴 red     far → 🔵 blue   (red→blue ramp, e.g. inverted turbo)
```

Works identically for camera and lidar (both are just points), so the live "device
depth view" is uniform across sources. near/far thresholds auto from the frame's
min/max range or a manual slider. Zero data cost (computed on GPU).

**Two color modes, toggleable** (extends "textured/untextured", `SPATIAL_3D_PLAN.md`
§3.2):
- **RGB / textured** — real camera colors (lidar would be grey/intensity here);
- **Depth heatmap** — red=near, blue=far, for ANY source → "I can see depth".

**Performance:** camera clouds can be dense (iPhone depth ~256×192 ≈ 49k pts; from
full RGB we decimate to a target density at the source). Color = +3 B/point on the
same i16/quant + LZ4 transport; the heatmap is free (GPU-side).

---

## 12. Phase 0 — chunk plan (LiDAR-only SLAM loop on Go2; codex review per chunk)

Prove the whole architecture on the tractable sensor we already drive live, in a new
pure-Rust crate `tentaflow-slam` (testable off-device; no GPU).

> **STATUS:** 0a–0f DONE in crate `tentaflow-slam` (36 tests, each chunk
> codex-reviewed). REMAINING = the LIVE core/dashboard hook + real-Go2 run, gated on
> (a) fixing the wgpu GPU-leak that crashes the instance, and (b) deciding how to feed
> Go2's pre-fused `voxel_map_compressed` (already a map, not raw scans) into LIO.

- **0a — Shared data model + frozen-submap invariant.** `tentaflow-slam` crate:
  `Pose(SE3)`, `SubmapId`, `Submap` (frozen geometry handle + keyframes), `Constraint`
  (Odometry|LoopClosure|Gnss|Georef|InterSubmap, with information matrix + status),
  `PoseGraph`, `Scene`. Unit tests: immutability of sealed submaps, constraint
  add-only, deterministic pose-solve gauge. (THIS chunk now.)
- **0b — LiDAR-inertial odometry.** Point-to-plane ICP (KISS-ICP style: voxel
  downsample + adaptive threshold), frame→active-submap tracking, IMU preintegration
  prior. Deps: `nalgebra` + a kd-tree (`kiddo`). Tested on synthetic + recorded Go2
  clouds.
- **0c — Submap sealing + frozen TSDF integration** (reuse SPATIAL voxel store).
- **0d — Pose-graph backend.** Evaluate `factrs`; loop-closure factor with §5 gating
  (geom verify + χ² + 2nd confirmation); incremental optimization → repositioned
  submaps. Fallback: hand-rolled sparse GN; escalation: gtsam FFI.
- **0e — Wire into core.** Consume Go2 canonical lidar frames → SLAM → canonical
  `GlobalPose` stream (lat/lon/alt once georeferenced, else scene-local + covariance).
- **0f — 2-node mesh merge.** Replicate submaps + constraints over the sync ledger;
  confirm both nodes derive convergent submap poses.

---

## 13. Work distribution & multi-node roles (no node does everything, no node idles)

Governing rule: **light work follows the sensors; heavy-global work has ONE owner
per scene; heavy-offloadable work goes to whichever node is free + capable.** Nothing
is computed N× "just because".

**Pinned to the node a robot is connected to (local, latency-bound):**
- fast-path tracking (per-frame odometry) — the sensor stream is there;
- submap production (accumulate geometry, seal, keyframes, descriptors); that node is
  the submap's `origin_node`.
So robots on node A are tracked+mapped by A, robots on B by B — work spreads with the
sensors automatically.

**Shared = replicated facts (the "common point map"):** submaps (immutable,
content-hashed) + constraints + poses flow over the existing sync ledger. The scene's
map is the UNION of every node's submaps, placed by the shared pose graph. A and B
each publish theirs → both hold the whole map.

**One owner per scene (avoid N× compute):** the heavy GLOBAL work — pose-graph
optimization, cross-submap loop-closure search, the scene VPR index — runs on a single
elected coordinator node per scene; it publishes resulting submap poses as facts that
others consume cheaply.
- Election: deterministic, capability-weighted (strongest live node; ties → lowest id).
- Failover: deterministic derivation (§6) means a new owner recomputes the identical
  result — no handoff state.
- Different scenes → different owners → natural load spread (building 1 optimized on A,
  building 2 on B).

**Offloadable heavy tasks (so node B isn't idle even with all robots on A):** depth-model
ONNX inference (for cameras), VPR descriptor computation, scan registration / loop
geometric verification, global optimization — are dispatched to the least-loaded
*capable* node via mesh-command, exactly like the existing `WebResearch` offload (to a
node with SearXNG) and cross-node frame pickup. So A (CPU-bound ingesting+tracking)
pushes depth-inference + optimization to an idle GPU server B; B earns its keep as a
compute sink with zero robots of its own.

**Capability-aware roles:** phone / robot / drone = thin (ingest + fast tracking, light
ESKF, offload the rest); server (GPU) = heavy (optimization, VPR index, depth models,
georeference). A drone on node A offloads its vision/depth to beefy node B.

**Concrete A+B:** same building → same scene → submaps from A and B land in one scene →
the scene owner finds loop closures BETWEEN A's and B's submaps and stitches them → one
coherent map. Different buildings → different scenes → never fused (§SPATIAL-11).

---

## 14. Mesh transport — channels = QUIC streams per traffic class + priorities

Today: one iroh/QUIC connection per peer (UFP/2 framed channels). Refinement so a bulk
transfer can't block a control message:

- **Do NOT** use N fixed connections or a hardcoded "6 channels". QUIC streams are
  already mutually non-blocking (loss on one never stalls another) — use **one stream
  (class) per traffic type**; it is strictly more flexible.
- **Head-of-line blocking is handled per level:** across streams → QUIC isolates;
  within a stream → ordered bytes, so **bulk transfers get one stream PER transfer**
  (a submap blob = open→send→close, never queued behind another); connection level →
  all streams share one bandwidth/congestion controller, so use **stream priorities**.
- **Traffic class → transport:**
  | class | nature | transport |
  |------|--------|-----------|
  | control / mesh-command / election | small, latency-critical | high-priority reliable stream (req/resp) |
  | pose / telemetry / live point-cloud preview | ephemeral, latest-wins | **QUIC datagrams** (no retransmit — stale data is useless) |
  | video | real-time | own stream / datagrams (existing path) |
  | map sync (submaps, blobs, constraints) | bulk, reliable, throughput | one stream per transfer, low priority, backpressured |
- **Priorities, not round-robin.** Round-robin gives equal share; we want UNEQUAL: a
  50 KB control message must preempt a 5 MB submap, not get an equal slice. Order:
  **control > real-time (pose/preview) > video > bulk sync.** Round-robin only as a
  crude fallback if priorities are unavailable.
- Maps onto existing UFP/2 channels: each logical channel = a separate QUIC stream;
  add per-class priority; use datagrams for ephemeral real-time.
