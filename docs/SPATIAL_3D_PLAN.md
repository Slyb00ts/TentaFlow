# Unified Spatial / 3D System — Plan

Status: REVIEWED BY CODEX. §10 holds the locked decisions + must-fixes + the
reordered (storage-first) chunk plan, and supersedes earlier tentative choices where
they differ. Owner: spatial pipeline.
Builds on the live LiDAR pipeline (canonical `LidarFrame` + host hub + binary push,
see `project_lidar_pipeline`). This generalizes it into a device-agnostic spatial
system that fuses, stores, and serves a real 3D reconstruction.

## 0. Goal & guiding principle

Many heterogeneous sources — Unitree Go2 voxel LiDAR, raw spinning/solid LiDAR,
iPhone (ARKit depth + VIO), camera-only (metric-depth model) — must feed ONE
unified system that a 3D engine later draws. Split of responsibility (matches the
existing addon=driver / core=generic model):

- **Addon = driver**: device-specific → **unified** (`SpatialFrame`: posed points +
  metadata). All vendor quirks live here.
- **Core = unified system**: registry + transport + fusion + **persistence** +
  query/stream API. Device-agnostic.
- **Engine (later, wgpu)**: draws unified layers. Knows nothing about devices.

**Top priority: PERFORMANCE of the whole loop — ingest, fuse, FIT, and especially
SAVE/LOAD.** Everything is designed sparse, chunked, incremental, zero-copy.

The core insight: unified ≠ "a point cloud". Unified = **posed geometry** — points
in the sensor frame + the sensor→world pose — so fragments from every source can be
placed in one world frame and re-placed after pose optimization without re-fetching
points.

## 1. Scope — Phase 1 (THIS plan) vs later

Phase 1 delivers EVERYTHING except the wgpu drawing:

- Unified `SpatialFrame` contract + `SpatialSource` descriptor (sdk-spec).
- Native core **spatial engine**: ingest posed frames → fuse into a sparse
  volumetric scene; derive the representation layers below.
- **Layered scene model** the engine can request:
  1. **Raw voxels** (occupancy + optional color) — the "surowe dane" view.
  2. **Textured / untextured** — color layer is separable (geometry-only fast path,
     or geometry+texture).
  3. **Grouped / primitives** — detected structure fitted to shapes: walls/floor/
     ceiling → planes (straight), pillars → cylinders, objects → boxes/spheres,
     with dimensions. A wall becomes one plane, not 10k voxels.
- **Persistence**: insanely-efficient incremental SAVE + fast random/streaming
  READ of the entire scene (voxels + poses + primitives + textures).
- **Query/stream API** the future engine consumes (by region, by layer, by LOD).
- Validation WITHOUT wgpu: tests + a CLI that dumps stats / exports a region
  (e.g. to PLY/OBJ) to prove correctness.

Later phases (out of scope here, listed for context):
- Phase 2: pose quality — odometry/LIO/VIO per source, anchors (AprilTag),
  cross-registration, common world frame, loop closure / pose-graph (drift).
- Phase 3: wgpu browser engine rendering the layers (raw voxels, textured mesh,
  primitives), LOD streaming.

> Honesty: the unified FORMAT is the easy ~20%. Good POSES + fusion (Phase 2) are
> the hard 80%. Phase 1 stands up the machinery and the storage so Phase 2/3 plug in.
> In Phase 1, sources carry whatever pose they natively have (iPhone VIO, robot
> odom, or identity); fusion is "integrate at the given pose" — good enough to build
> and benchmark the whole storage/representation stack.

## 2. Unified contract

### 2.1 `SpatialFrame` (per-frame, what addons emit, fixed binary)

```
Header (fixed LE):
  version u8, source_id (interned u32 + a registry), frame_seq u32, timestamp_us i64
  point_count u32, layout (XYZ | XYZI | XYZRGB | XYZ+label), stride u8
  units = meters; coordinate convention = right-handed, Z-up (CANONICAL — addons normalize)
Pose (sensor → world, for THIS frame):
  translation x,y,z f32 ; rotation quaternion qx,qy,qz,qw f32
  scene_id (interned) + frame_id/parent_frame_id  ; pose_source (robot_odom|vio|lio|anchor|none) u8
  pose_confidence f32 (+ optional 6x6 covariance, later)
  points_are_world bool       (false = points in sensor frame; fuser applies pose)
Body: packed f32, points in SENSOR frame; optional interleaved channels
  (intensity f32 | RGB u8x3 | semantic_label u16 | per-point confidence u8)
```

Decision: **points stay in the sensor frame + carry the pose** (not pre-baked to
world). Lets Phase-2 loop-closure re-optimize poses and re-render WITHOUT touching
the stored points. Standard submap + pose-graph separation.

### 2.2 `SpatialSource` (registered once)

```
source_id, kind (lidar_360 | lidar_solid | depth_cam | rgb_only)
intrinsics / FOV / range_min,max / native_rate_hz
self_poses bool (iPhone VIO=true, raw lidar=false → needs external odom)
default_confidence (lidar > camera-depth) ; color_capable bool
```

This is the only device-specific surface the core sees. Mirrors the
`robot_dispatch` registry pattern.

## 3. Layered scene model (core, native)

All layers derive from the same fused volumetric store; the engine picks what it
needs.

### 3.1 Volumetric core — block-hashed sparse voxels (the workhorse)

Chosen representation: **block-hashed sparse voxel grid** (the voxblox/nvblox
design), Rust-native, no heavy C++ dep:

- World = `HashMap<BlockCoord, Block>`; `Block` = dense `N×N×N` voxels (e.g. N=16).
- Per voxel: occupancy/weight, **TSDF** (signed distance, for surfaces/mesh),
  optional packed RGB + color weight, optional semantic label.
- Block = the unit of: persistence, meshing, primitive fitting, streaming, dirty
  tracking. Only touched blocks allocate (sparse), only changed blocks re-process.
- Voxel size configurable per scene (e.g. 2–5 cm). Multi-resolution / LOD via
  coarser block summaries.

Why not OpenVDB/NanoVDB directly: VDB is the gold standard for sparse volumes and
NanoVDB is GPU-friendly for the engine, but it's a large C++ dependency. Block-hash
gives ~the same sparsity/perf in pure Rust and maps cleanly to NanoVDB later for the
GPU read path if needed. (Open question for codex — see §8.)

### 3.2 Surface + texture layer (textured / untextured)

- **Marching Cubes per block** over the TSDF → triangle mesh (incremental: only
  dirty blocks re-mesh). Untextured view = this mesh.
- **Texture/color**: two modes —
  (a) per-voxel/vertex RGB (cheap, from projecting camera frames onto voxels), or
  (b) per-block UV texture atlas baked from camera images (higher quality).
  Color layer is SEPARABLE: geometry-only render skips it entirely (fast path).
- Camera→voxel projection: for each camera `SpatialFrame` (RGB + pose), project
  pixels onto visible voxels/surface, accumulate color with confidence weighting.

### 3.3 Primitive / semantic layer (grouped)

Compact, clean, and a big perf/storage win:

- **Planes** (walls/floor/ceiling): RANSAC plane segmentation + region growing over
  blocks → planar patches with extent & normal. Snap near-vertical→walls,
  near-horizontal→floor/ceiling → "straight" surfaces instead of noisy voxels.
- **Cylinders / boxes / spheres** (objects): RANSAC/cluster fitting on
  non-planar clusters → primitive params + dimensions (radius, w×h×d).
- **Detection tie-in**: TentaVision detectors (YOLO/RF-DETR) + metric depth seed
  object clusters with a class → semantic object = {class, fitted primitive,
  dimensions, pose, confidence}.
- Stored as a separate small primitive list per region; updated incrementally as
  blocks change. The engine can render primitives INSTEAD of voxels for a clean,
  ultra-cheap view.

## 4. Persistence — efficient save/load (CRITICAL)

Requirements: fast INCREMENTAL write during live scanning; fast READ (random
access by region + streaming to the engine); whole-scene save/restore.

Design — **chunked, append-log + snapshots, per-block compression, mmap reads**
(mirrors TentaFlow's Sync-Ledger pattern):

- **Spatial chunking**: the world is tiled into chunks (a chunk = a set of blocks
  in a region). A chunk is the load/save/stream unit → partial load, streaming
  near the view, no full rewrite on small updates.
- **Write path**: live integration marks dirty blocks; a background flusher
  appends compressed block deltas to a per-chunk **append log** (sequential
  writes = fast). Periodic **snapshot/compaction** folds the log into a compact
  chunk file (like ledger snapshots). Crash-safe (log replay).
- **Read path**: open snapshot + replay tail; **memory-map** chunk files for
  zero-copy random access; decompress per block on demand.
- **Compression**: per-block Zstd/LZ4 (occupancy/TSDF/color compress very well;
  the robot already LZ4s voxels). Tune for write speed vs ratio.
- **Layers persisted**: voxel blocks (geometry+TSDF), color blocks (separate file
  → load textured or not), primitive layer (tiny), source/pose log (the trajectory
  + `SpatialFrame` poses, so the scene can be re-fused/re-optimized in Phase 2).
- **Two artifacts**: (1) the live working store (block-hash + chunk files), (2)
  optional exports (PLY/OBJ/glTF for a region, NanoVDB for GPU) — Phase-1 CLI uses
  these to validate without wgpu.

Performance levers throughout: sparse (only touched blocks), incremental (only
dirty blocks re-mesh/re-fit/re-persist), chunk-parallel (Rayon), SIMD voxel
integration, lock-light (per-block / per-chunk locks, latest-wins like the hub),
zero-copy mmap, GPU-friendly output (packed buffers / NanoVDB).

## 5. Processing pipeline (per incoming `SpatialFrame`)

1. Decode + (if `points_are_world=false`) transform by pose → world.
2. **Integrate** into the block-hash TSDF/occupancy (+ color if RGB) — sparse,
   SIMD, only touched blocks; mark them dirty.
3. **Append** dirty-block deltas to the chunk log (background flusher).
4. (Background, throttled) **re-mesh** dirty blocks (marching cubes).
5. (Background, throttled) **re-fit primitives** for regions whose blocks changed.
6. (Background) **project camera color** onto affected voxels.
7. **Serve** any layer to the engine via the query/stream API.

Each stage is independent + throttled so live ingest is never blocked by meshing/
fitting/persistence (decouple like the LiDAR tick vs decode).

## 6. Query / stream API (what the engine will consume)

- Subscribe to a region (bbox / frustum) at a LOD → stream of: raw voxel blocks,
  OR mesh+texture, OR primitives — engine picks the layer.
- Latest-wins per block (like the LiDAR hub) for live; full region load for replay.
- Binary, zero-copy where possible; reuses the existing host hub + push transport
  generalized from `lidar_hub`/stream dispatch.

## 7. Where it lives in TentaFlow

- **Native core service** (Rust) — all heavy work (integration, meshing, fitting,
  persistence) is native, NOT in a WASM addon (too heavy / fuel-limited). Addons
  only produce `SpatialFrame`s.
- **Spatial registry + hub + spatial store** in core (generalize `lidar_hub` →
  `spatial_hub`; add the chunked store).
- **go2 addon** = first `SpatialSource` (voxel → points + pose from robot odom).
- Persistence dir under the runtime home (configurable; large data NOT in SQLite).

## 8. Open questions for codex

1. Block-hash sparse voxel + per-block TSDF vs adopting OpenVDB/NanoVDB now (Rust
   binding cost vs GPU-readiness vs reinventing)? Best Rust-native sparse-volume
   approach for max ingest+save throughput?
2. Persistence: append-log + snapshot per chunk vs a single embedded KV (Fjall,
   already in the stack) keyed by block coord vs mmap'd chunk files? Which gives
   the best incremental-write + random-read + crash-safety tradeoff?
3. Color/texture: per-voxel RGB vs per-block UV atlas — memory/perf vs quality for
   the on/off-texture requirement.
4. Primitive fitting cadence + method (RANSAC vs region-growing vs learned) to keep
   it real-time and incremental without thrashing as blocks update.
5. Coordinate/units canonical choice (Z-up RH?) and how to keep multi-source frames
   consistent before Phase-2 anchors exist.
6. Is splitting points-in-sensor-frame + pose worth the per-frame transform cost vs
   storing world points, given Phase-2 loop-closure needs re-optimization?
7. Biggest perf risk in the save/load path and how to de-risk it early.

## 9. Phase-1 chunk plan (implementation order, codex review per chunk)

- S1 — `SpatialFrame` + `SpatialSource` types in sdk-spec (extend `LidarFrame`);
  hub/push/protocol-wasm generalized; go2 emits `SpatialFrame` with a pose.
- S2 — native block-hash sparse voxel store + TSDF integration (sparse/SIMD), unit-
  benchmarked for ingest throughput.
- S3 — chunked persistence (append-log + snapshot + mmap + per-block compression);
  save/load benchmarked (THE critical perf gate).
- S4 — surface layer (marching cubes per dirty block) + color layer (separable).
- S5 — primitive/semantic layer (planes for walls/floor, cylinders/boxes/spheres;
  detection tie-in).
- S6 — query/stream API + a CLI validator (region export PLY/OBJ, stats) — proves
  the whole Phase-1 end to end WITHOUT wgpu.

Each chunk: implement → benchmark (perf is the gate) → codex review → next.

> NOTE: §10 (codex review) SUPERSEDES the tentative choices above where they differ
> — especially the ingest path, the `SpatialFrame` fields, and the chunk ORDER
> (storage benchmark moves to the front).

## 10. Codex review — verdict, must-fixes, locked decisions

Verdict: direction is sound (addons normalize, core fuses/stores, renderer dumb).
But Phase 1 as drafted is over-scoped and the dangerous abstraction is reusing the
**latest-wins hub** for reconstruction.

### 10.1 MUST-FIX

1. **Split preview from reconstruction ingest.** Latest-wins (the LiDAR hub) is fine
   for live PREVIEW, but WRONG for mapping — dropped frames = lost geometry and lost
   free-space evidence. Reconstruction needs a **bounded, ordered frame queue / WAL
   with backpressure + metrics**, not latest-wins. Two distinct paths.
2. **`SpatialFrame` needs full sensor-model metadata** (TSDF free-space carving needs
   it): ray/sensor origin, range_min/max, beam model, depth-cam intrinsics,
   invalid-depth semantics, per-point confidence, and a flag for hits-only vs
   occupied-voxels. Without ray origin you can't carve free space correctly.
3. **Poses = `f64`** in metadata/storage (global `f32` poses break on large maps).
   Keep voxel-local coords + TSDF/weights compact `f32`/quantized.
4. **Design loop-closure invalidation NOW (submaps + provenance).** Weighted TSDF
   fusion is not reversible. Frames integrate into **local submaps**; store per-
   frame/submap pose history + provenance so Phase-2 loop closure can MOVE submaps
   and selectively reintegrate — instead of throwing the whole map away.

### 10.2 Locked decisions (answers to §8)

1. **Volume:** Rust-native **block-hash sparse TSDF/occupancy** now. NOT OpenVDB
   (C++ in the hot path) / NOT NanoVDB as the write store (it's a GPU read format).
   chunk → blocks → `16³` voxels; hot memory **SoA** (not struct-per-voxel);
   quantized TSDF/weights for storage.
2. **Persistence:** **custom append-log + immutable per-chunk snapshots**; append
   compressed block records sequentially, compact to immutable chunk files, **mmap
   the immutable snapshots** for reads, replay tail log on open. **Fjall only for
   metadata/index**, NOT high-rate voxel payloads (KV-per-block = write
   amplification + compaction stalls + uncontrolled random IO). Pure mutable mmap =
   wrong for crash safety/resize.
3. **Color:** Phase 1 = **per-voxel/per-vertex RGB only**, separate color block
   stream, geometry load independent of color. NO UV atlas yet; persist camera
   keyframes separately so atlas can be added later without touching geometry.
4. **Primitive fitting:** **planes first**, objects second; **region/chunk debounce,
   not per-frame**. Planes: RANSAC seed + region growing + PCA/SVD refine. Objects:
   cluster non-planar residuals → cheap OBB / sphere / cylinder, update only
   affected regions. No learned fitting in Phase 1.
5. **Coordinates:** Z-up RH, but ALSO need `frame_id` + `parent_frame_id` +
   per-source extrinsics + timestamp domain + calibration id + pose
   covariance/confidence + enforced units. Before anchors/loop-closure, **only fuse
   sources in the same validated world frame**; otherwise keep them as separate
   submaps (don't blindly fuse unrelated frames).
6. **Sensor-frame points + pose:** correct — but ALSO store submap pose history +
   provenance (don't keep only fused global blocks), so affected chunks can be
   rebuilt after pose optimization.
7. **Biggest save/load risk:** NOT disk bandwidth — it's **millions of tiny
   block records** (serialize/compress/hash/index CPU + cache + metadata overhead).
   De-risk by building the **persistence benchmark BEFORE primitive fitting**:
   synthetic dirty-block stream, realistic block sizes, crash replay, cold whole-
   scene load, random-bbox load, texture-off and texture-on load.

### 10.3 Additional gaps to incorporate

dynamic-object policy; chunk checksums + schema version + magic + endian + index
format; explicit per-block compression target; explicit perf budgets; calibration
lifecycle; time-synchronization story; clear preview-stream vs durable-scene split.

### 10.4 Crates / algorithms (Rust)

- Volume/transforms: `hashbrown`/`ahash` (or sharded maps), `parking_lot`, `rayon`,
  `glam` or `nalgebra`; SoA + quantized voxels.
- Surface: custom marching-cubes over dirty blocks; benchmark `fast-surface-nets`;
  `meshopt` later for simplification/packing.
- Fitting: RANSAC + region-growing + PCA/SVD (planes), Euclidean clustering + PCA
  OBB (boxes), RANSAC + least-squares (cyl/sphere); `kiddo`/`rstar` for kd/r-tree;
  `nalgebra` math.
- Compression: `lz4_flex` (hot logs), `zstd` (cold snapshots); `xxhash-rust`/
  `crc32fast` checksums.
- IO/binary: `memmap2`; `bytemuck`/`zerocopy` POD block payloads; explicit LE
  headers; NO serde in hot block payloads.

### 10.5 Phase-1 chunk plan — REORDERED (storage-first; replaces §9 order)

Storage is the #1 requirement → it must be the first serious benchmark gate.

- **S0** — define block/chunk binary format (magic, version, endian, checksums,
  index), perf budgets, and a **synthetic storage benchmark harness**.
- **S1** — `SpatialFrame` + `SpatialSource` with calibration/frame/sensor-model
  metadata (f64 poses) + a **durable ordered ingest queue/WAL** (not latest-wins).
- **S2** — block-hash voxel store (SoA), chunk coords aligned with persistence.
- **S3** — persistence prototype (append-log + snapshot + mmap), crash-replay tests,
  synthetic benchmarks. ← THE critical perf gate.
- **S4** — minimal TSDF/occupancy integration from Go2/iPhone; measure dirty-block
  rate against budget.
- **S5** — query API + CLI region export / load stats (proves Phase 1 sans wgpu).
- **S6** — surface extraction (marching cubes / surface nets over dirty blocks).
- **S7** — primitive/semantic layer (planes → objects, debounced).
- **S8** — color layer (per-voxel/vertex RGB, separable).

Keep preview (latest-wins live stream) and the durable reconstruction store as two
separate paths throughout.

## 11. Multi-site / multi-scene scoping (NO single global world)

Hard requirement: robots may be in **different buildings**. There is **no one global
visualization**. The system manages **many independent scenes**, and fusion happens
ONLY within a scene.

- **Scene = the unit of one coherent reconstruction** — its own coordinate origin,
  anchors, voxel/TSDF store, primitive layer, pose graph, and persistence partition.
  Typically one building or one floor.
- **Hierarchy** for organization + LOD: `site → building → floor → area`. The
  FUSION unit is the scene (building/floor); the hierarchy is metadata for browsing
  and coarse LOD, not a cross-building merge.
- **`SpatialFrame.scene_id`** binds every frame to its scene; `frame_id`/
  `parent_frame_id` give the transform tree WITHIN the scene. A `SpatialSource` is
  assigned to a scene at config/install time (where the robot physically is) — like
  the per-instance IP.
- **Everything scene-scoped**: integration, registration, anchors, loop-closure,
  the fused map, and the query/stream API. **Cross-scene sources are NEVER fused.**
  Two robots in the same building share a scene → one map; robots in different
  buildings → separate scenes → separate maps.
- **Persistence partitioned per scene** (separate chunk namespace/dir per scene) →
  load, stream, snapshot, and evict one scene independently; a node never has to
  load unrelated buildings.
- **Mesh / multi-node**: the spatial registry tracks `scene_id` per source; a fuser
  instance runs per ACTIVE scene; scenes can live on different nodes; a building's
  robots (even across nodes) feed that building's scene. A node hosts only the
  scenes it owns/needs.
- **Visualization**: the engine selects ONE scene (or a sub-scope like a floor) —
  never a global cross-building view. Switching scene = load that scene's store.
- **Edge cases**: a robot that moves to another building → reassigned to a new scene
  (new world, no carry-over of the old map's frame). A source with unknown location
  → its own provisional scene until anchored/assigned.

This composes cleanly with §10: the durable store, ingest WAL, submaps, and pose
graph all already key by scene; we just make `scene_id` a first-class partition key
everywhere (format, store path, registry, query). S0/S1 must bake `scene_id` into
the block/chunk format and the ingest queue from the start.

## 12. Localization WITHOUT markers (anchors are optional)

Hard requirement: rooms MAY but need NOT have markers; devices must still find
themselves in the scene. So **markerless is the DEFAULT path and must work on its
own**; anchors are an optional accelerator, never assumed.

- **Anchors (AprilTag / QR / UWB) = optional constraints.** When present they give
  cheap, instant, drift-free absolute alignment. When absent, the system relies
  entirely on markerless methods below. Anchors are just one more edge type in the
  scene pose graph — never a prerequisite.
- **Default markerless localization:** each device runs odometry/SLAM (LiDAR-inertial
  for robots/raw LiDAR; ARKit VIO for iPhone; visual(-inertial) for camera-only) →
  its trajectory in its own frame. The COMMON scene frame comes from **overlap-based
  registration**: place recognition (e.g. Scan Context / learned global descriptors;
  visual bag-of-words for cameras) + point-cloud registration (GICP / TEASER++) +
  loop closure in the scene pose graph.
- **Relocalization (a device "finds itself"):** when a device enters/re-enters a
  scene, it matches its current observation against the scene's existing fused map
  via markerless place-recognition + registration → snaps into the scene frame. No
  marker needed; works for the second robot joining a building the first one mapped.
- **Alignment fallback order per new/returning source:**
  1. anchor seen → use it (instant absolute);
  2. else relocalize markerlessly vs the scene's existing map (place-rec + register);
  3. else (no anchor, no overlap yet) → keep it as its OWN provisional submap and
     merge automatically once overlap appears (or it sees an anchor). It is never
     blindly fused into an unrelated frame.
- **Honest limits (inherent, not a defect):** markerless alignment needs sufficient
  OVERLAP and geometric/visual STRUCTURE. Two devices that never see a shared region,
  or a long featureless corridor, can drift or fail to relate until overlap/structure
  appears — exactly the case where an optional marker removes the ambiguity. So:
  markerless first; markers offered as an optional reliability boost where the
  environment is degenerate. Cameras add visual place-recognition to help LiDAR-poor
  geometry, and vice-versa.

Architecture/format already support this: `pose_source` covers `lio|vio|anchor|none`,
poses carry confidence/covariance, submaps + the scene pose graph absorb anchor and
markerless constraints uniformly. Phase 2 implements the markerless registration +
relocalization first; anchor support is an additive constraint type on top.

## 13. Canonical frame layout — DECISION (wire-only; decoder normalizes)

Settled while optimizing the live Go2 LiDAR path; applies to every source.

- **`layout` is a WIRE concern only.** The per-frame header `layout` byte tells the
  DECODER how to read the body; it is NOT a contract the renderer sees. The decode
  boundary (`decodeLidarFrame` in wasm) ALWAYS outputs world-space `points` as a
  `Float32Array`, regardless of how the body was packed. The renderer therefore
  never branches on layout — input is uniform. Heterogeneous wire → decoder
  unifies → homogeneous renderer. (`raw` still carries the canonical bytes if a
  future GPU path wants to upload compact data and expand in a shader.)
- **Layouts are per-source, each lossless for its data:**
  - `XYZ` (3×f32) / `XYZI` (4×f32): continuous sources (raw lidar, iPhone ARKit) —
    full precision.
  - `XYZ_I16_PLANAR` (tag 6): grid-aligned sources (voxel maps, e.g. Go2). i16 grid
    indices, planar (all `ix`, then `iy`, then `iz`). LOSSLESS for grid data; half
    the bytes; planar so each plane is a low-entropy run for the compressor.
  - Future `XYZ_Q16` (quantized i16 + per-frame scale/offset): continuous sources
    where quantization to sub-sensor-noise precision is acceptable (e.g. iPhone
    depth noise ~1 cm ≫ mm-grid) — near-lossless, half the bytes. f32 stays the
    escape hatch for unusual dynamic range. Submap-local origins (§11) keep i16
    range sufficient even at building scale.
- **LZ4 (`LIDAR_FLAG_LZ4_BODY`) is a UNIVERSAL, LOSSLESS wire compression on TOP of
  any layout.** Applied host-side (native, off the metered addon tick), it never
  touches quality and works for f32 and i16 alike. Header stays uncompressed so the
  host can stamp `host_send_us` and the decoder can size the inflate buffer.
- **Point COUNT is a separate axis from per-point bytes.** Density is managed by
  source-side voxel decimation in the per-device addon (target a max points/frame),
  NOT by the wire format. So a high-resolution sensor (e.g. dense aerial scan) does
  not imply fat frames.
- **Measured effect (live Go2, ~45k pts):** f32 → i16 halved bytes (decode 2 ms →
  0.4 ms via bulk `Float32Array`); + LZ4 on planar i16 compresses the low-entropy
  planes far better than interleaved. Net latency dropped from ~20 ms (f32) toward
  single-digit ms. The lidar stream is also lossy latest-wins server-side (a slow
  consumer never backs up the queue nor force-disconnects).

## 14. Universal positioning / localization

The map layer here is the GNSS-denied backbone for *knowing where any device is*
(indoor/outdoor/air/ground, any sensor mix, robust to RF jamming). That system —
multi-sensor → global pose fusion, georeferenced map-relative relocalization,
self-location — is designed in **`docs/UNIVERSAL_POSITIONING_PLAN.md`**. It reuses
this plan's georeferenced scenes (§11), markerless relocalization (§12) and the
canonical frame/decoder split (§13).
