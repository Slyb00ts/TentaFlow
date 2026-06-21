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
- **Pose graph is DERIVED, not synced**: every node runs the SAME deterministic
  optimizer over the SAME merged (submaps, constraints) set → the same submap poses
  (up to a gauge fixed by a convention, e.g. anchor the georeferenced submap, or the
  lowest-id submap at identity). This mirrors the existing "materialization already
  convergent; admission is the only thing to control" design — poses converge because
  they are a pure function of replicated facts.
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
