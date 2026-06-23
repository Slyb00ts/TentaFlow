// =============================================================================
// File: lib.rs — browser WebGPU/WebGL voxel / occupancy-grid SLAM viewer
// Live 3D view of a robot's LiDAR cloud (~30-47k points/frame) drawn as instanced
// voxel cubes (Z-up) colored by horizontal radial distance from the robot (magenta
// near -> green at the edges), each cube outlined with a dark Minecraft-style edge,
// on top of a wireframe ground grid, with a small robot marker placed at the robot's
// world pose and mouse orbit/zoom.
//
// The Go2 voxel_map arrives ALREADY in a fixed odom world frame (the grid origin is
// constant across frames; the robot moves through it). We therefore accumulate the
// map as the UNION of occupied voxel cells across frames — quantized to the voxel
// resolution — so a partial single-frame cloud grows into a full map as the robot
// moves. The robot's world pose comes from a separate topic via `setRobotPose`.
// =============================================================================

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use glam::{Mat4, Quat, Vec3};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wgpu::util::DeviceExt;

// Clear color tuned for a dark dashboard (~#0b0f17), close to the reference
// occupancy-grid viewer's dark gray backdrop.
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.043,
    g: 0.059,
    b: 0.090,
    a: 1.0,
};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

// Initial radial range in meters until the first cloud sets an adaptive range
// from its actual horizontal extent.
const HEATMAP_RANGE_METERS: f32 = 8.0;

// Sparse LiDAR reads as isolated points at the 0.05 m resolution; rendering the
// cube slightly larger than the voxel pitch makes adjacent occupied cells visually
// merge into solid blocks like the reference occupancy grid. The JS-passed
// `voxelSize` keeps its meaning (the true cell pitch); this fill factor is applied
// internally to the rendered cube edge only.
const VOXEL_FILL_FACTOR: f32 = 1.0;

// Maximum number of accumulated occupied voxel cells. 400k instanced cubes draws
// comfortably; past this we evict the oldest-inserted cells (FIFO) so the map stays
// bounded as the robot explores. Also the hard cap on the instance buffer capacity.
const MAX_ACCUMULATED_CELLS: usize = 400_000;

// Ground grid: cell size and how far the grid is padded beyond the cloud's X-Y
// extent, both in meters.
const GRID_CELL_SIZE: f32 = 0.5;
const GRID_PADDING: f32 = 1.0;
// Subtle gray for the wireframe grid lines.
const GRID_COLOR: [f32; 3] = [0.22, 0.22, 0.22];
// Rebuild the grid only when the floor footprint changes by more than this many
// meters on any edge, so we don't recreate the vertex buffer every frame.
const GRID_REBUILD_EPSILON: f32 = 0.25;

// -----------------------------------------------------------------------------
// GPU data layout
// -----------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CubeVertex {
    position: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    // World-space translation of the voxel center.
    translation: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LineVertex {
    position: [f32; 3],
    color: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SolidVertex {
    position: [f32; 3],
    color: [f32; 3],
}

// Robot-mesh vertex: position + real glTF vertex normal + per-link flat color.
// Drawn through the dedicated robot pipeline for smooth normal-based Lambert
// shading (unlike the grid's flat-colored LineVertex).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RobotVertex {
    position: [f32; 3],
    normal: [f32; 3],
    color: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    // Cloud horizontal center the radial colormap is measured from (X-Y used;
    // Z is ignored so the floor reads as one radial field) + inverse range.
    heatmap_origin: [f32; 3],
    inv_heatmap_range: f32,
    // Rendered cube edge length in meters (voxel pitch * fill factor).
    voxel_size: f32,
    _pad: [f32; 3],
    // Overlay mode + fixed color: `[mode, r, g, b]`. mode>0.5 makes the vertex
    // shader use the fixed RGB instead of the radial colormap, so a second cloud
    // (camera depth) renders in one distinct colour over the lidar map.
    overlay: [f32; 4],
}

// Uniform for the robot marker: a world-space model transform applied on top of
// the shared view_proj. The grid renders in world space directly and
// reuse only view_proj from the same buffer (model is identity for them).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ModelUniforms {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
}

// Robot-mesh uniform: the shared view_proj, the link's world model matrix, and the
// normal matrix used to transform vertex normals into world space. Go2 joints are
// pure rotations + translations, so the model's upper-3×3 is orthonormal and can be
// used directly as the normal matrix — we still pass it explicitly (as a mat4 whose
// upper-3×3 is the rotation, translation column zeroed) so the shader never has to
// reconstruct it. A mat4 keeps the std140 alignment trivial.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RobotModelUniforms {
    view_proj: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    normal_mat: [[f32; 4]; 4],
}

// Unit cube centered at origin, edge length 1 (scaled by voxel_size in shader).
#[rustfmt::skip]
const CUBE_VERTICES: [CubeVertex; 8] = [
    CubeVertex { position: [-0.5, -0.5, -0.5] },
    CubeVertex { position: [ 0.5, -0.5, -0.5] },
    CubeVertex { position: [ 0.5,  0.5, -0.5] },
    CubeVertex { position: [-0.5,  0.5, -0.5] },
    CubeVertex { position: [-0.5, -0.5,  0.5] },
    CubeVertex { position: [ 0.5, -0.5,  0.5] },
    CubeVertex { position: [ 0.5,  0.5,  0.5] },
    CubeVertex { position: [-0.5,  0.5,  0.5] },
];

#[rustfmt::skip]
const CUBE_INDICES: [u16; 36] = [
    0, 1, 2, 2, 3, 0, // -Z
    4, 6, 5, 6, 4, 7, // +Z
    0, 4, 5, 5, 1, 0, // -Y
    3, 2, 6, 6, 7, 3, // +Y
    0, 3, 7, 7, 4, 0, // -X
    1, 5, 6, 6, 2, 1, // +X
];

// Fixed colour for the overlay (camera-depth) cloud — bright magenta, which the
// lidar map's red→blue radial colormap never reaches, so the two layers never
// blend into the same hue.
const OVERLAY_COLOR: [f32; 3] = [1.0, 0.0, 1.0];

const SHADER_SRC: &str = r#"
struct Uniforms {
    view_proj: mat4x4<f32>,
    heatmap_origin: vec3<f32>,
    inv_heatmap_range: f32,
    voxel_size: f32,
    overlay: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) translation: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    // Cube-local position in [-0.5, 0.5]; the fragment uses it to draw a dark
    // edge outline on every voxel without extra geometry.
    @location(1) local_pos: vec3<f32>,
    // World position; the fragment derives a flat per-face normal from its screen
    // derivatives for cheap directional shading (no per-vertex normals needed).
    @location(2) world_pos: vec3<f32>,
};

// Compact HSV->RGB (h,s,v in [0,1]). Used for the radial-distance colormap so
// the cloud reads as a rainbow field centered on the robot.
fn hsv2rgb(h: f32, s: f32, v: f32) -> vec3<f32> {
    let k = vec3<f32>(5.0, 3.0, 1.0) / 6.0;
    let p = abs(fract(vec3<f32>(h) + k) * 6.0 - 3.0);
    return v * mix(vec3<f32>(1.0), clamp(p - 1.0, vec3<f32>(0.0), vec3<f32>(1.0)), s);
}

// Radial-distance colormap: close to the robot = RED, sweeping through orange,
// yellow, green, cyan to BLUE at the far edges (the classic, readable depth ramp).
// t is normalized horizontal distance from the robot [0,1]; hue 0 (red) -> 0.66
// (blue).
fn radialcolor(t: f32) -> vec3<f32> {
    let c = clamp(t, 0.0, 1.0);
    let h = c * 0.66;
    return hsv2rgb(h, 0.95, 1.0);
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let world = in.translation + in.position * u.voxel_size;
    out.clip_position = u.view_proj * vec4<f32>(world, 1.0);

    // Overlay cloud (camera depth) renders in one fixed colour so it reads as a
    // distinct layer over the lidar map; the lidar map keeps the radial colormap.
    if (u.overlay.x > 0.5) {
        out.color = u.overlay.yzw;
    } else {
        // Color by HORIZONTAL RADIAL DISTANCE from the cloud center (X-Y only, Z
        // ignored). heatmap_origin carries the cloud center; inv_heatmap_range =
        // 1 / max horizontal radius so green reaches the outer edge.
        let dxy = in.translation.xy - u.heatmap_origin.xy;
        let t = length(dxy) * u.inv_heatmap_range;
        out.color = radialcolor(t);
    }
    out.local_pos = in.position;
    out.world_pos = world;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Dark edge outline (Minecraft-style block borders). A cube edge is where the
    // two largest of |x|,|y|,|z| are both near the 0.5 face. Sort the three
    // absolute components and test the second-largest against the edge band.
    let a = abs(in.local_pos);
    let m0 = max(a.x, max(a.y, a.z));   // largest (always ~0.5 on a face)
    let m2 = min(a.x, min(a.y, a.z));   // smallest
    let mid = (a.x + a.y + a.z) - m0 - m2; // middle component
    // Edge band: a subtle darkened border so each cube is separated without the
    // heavy hollow-box look. Narrow band, gently darkened.
    let edge = smoothstep(0.5 - 0.06, 0.5 - 0.03, mid);
    let edge_shade = mix(1.0, 0.55, edge);

    // Flat per-face directional shading: derive the face normal from the world
    // position's screen derivatives (each cube face is planar, so this is exact).
    // Gives the voxels solid 3D relief instead of flat stickers.
    let n = normalize(cross(dpdx(in.world_pos), dpdy(in.world_pos)));
    let light_dir = normalize(vec3<f32>(0.4, 0.5, 0.85));
    // abs() so shading is independent of the derivative-normal sign (which flips
    // with the backend's fragment-Y convention) — each face is relit by its angle
    // to the light, no dead black side.
    let diffuse = abs(dot(n, light_dir));
    let lit = 0.45 + 0.55 * diffuse; // ambient + diffuse
    let shade = edge_shade * lit;
    return vec4<f32>(in.color * shade, 1.0);
}
"#;

// Shader for per-vertex-colored line geometry (the ground grid). Flat color, no
// lighting — the grid is a wireframe overlay and the robot now has its own pipeline.
const COLORED_SHADER_SRC: &str = r#"
struct ModelUniforms {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> u: ModelUniforms;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip_position = u.view_proj * (u.model * vec4<f32>(in.position, 1.0));
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
"#;

// Dedicated robot-mesh shader: smooth Lambert shading from the meshes' real glTF
// vertex normals (transformed by the link's normal matrix), so the articulated Go2
// reads as a smoothly shaded model instead of the faceted derivative-lit hack.
const ROBOT_SHADER_SRC: &str = r#"
struct RobotUniforms {
    view_proj: mat4x4<f32>,
    model: mat4x4<f32>,
    normal_mat: mat4x4<f32>,
};

@group(0) @binding(0) var<uniform> u: RobotUniforms;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) world_normal: vec3<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let world_pos = u.model * vec4<f32>(in.position, 1.0);
    out.clip_position = u.view_proj * world_pos;
    // normal_mat is the model's upper-3×3 (orthonormal for the Go2's pure
    // rotation+translation joints), padded into a mat4 with a zeroed translation.
    let n = (u.normal_mat * vec4<f32>(in.normal, 0.0)).xyz;
    out.world_normal = n;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.world_normal);
    let light_dir = normalize(vec3<f32>(0.4, 0.5, 0.85));
    let diffuse = max(dot(n, light_dir), 0.0);
    // Directional Lambert + ambient, plus a faint hemispheric term keyed off the
    // up-facing component so the dark body never reads as pure black.
    let hemi = 0.5 + 0.5 * n.z;
    let lit = 0.35 + 0.65 * diffuse + 0.08 * hemi;
    return vec4<f32>(in.color * lit, 1.0);
}
"#;

// -----------------------------------------------------------------------------
// Orbit camera
// -----------------------------------------------------------------------------

struct Camera {
    target: Vec3,
    distance: f32,
    azimuth: f32,
    elevation: f32,
    aspect: f32,
}

impl Camera {
    fn new() -> Self {
        Self {
            target: Vec3::ZERO,
            distance: 10.0,
            // Oblique 3/4 view: a slight azimuth and ~28° elevation looking down
            // at the floor, matching the reference occupancy-grid screenshot.
            azimuth: 0.7,
            elevation: 0.5, // ~28.6°
            aspect: 1.0,
        }
    }

    fn eye(&self) -> Vec3 {
        // Z-up scene (LiDAR convention): elevation tilts the eye along +Z so the
        // floor (the X-Y plane) renders horizontal instead of edge-on. Azimuth
        // orbits in the horizontal plane.
        let cos_e = self.elevation.cos();
        let dir = Vec3::new(
            cos_e * self.azimuth.cos(),
            cos_e * self.azimuth.sin(),
            self.elevation.sin(),
        );
        self.target + dir * self.distance
    }

    fn view_proj(&self) -> Mat4 {
        let eye = self.eye();
        let view = Mat4::look_at_rh(eye, self.target, Vec3::Z);
        // Near/far scale with the orbit distance so close clouds and far clouds
        // both stay inside the frustum without hand-tuning.
        let near = (self.distance * 0.01).max(0.01);
        let far = self.distance * 100.0;
        let proj = Mat4::perspective_rh(60.0_f32.to_radians(), self.aspect, near, far);
        proj * view
    }
}

// -----------------------------------------------------------------------------
// Renderer state
// -----------------------------------------------------------------------------

// Footprint of the ground grid in world space, cached to avoid rebuilding the
// grid vertex buffer every frame.
#[derive(Clone, Copy)]
struct GridBounds {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
    z: f32,
}

// A single articulated link's GPU geometry + its parsed kinematic placement.
// `visual_origin` is the URDF <visual><origin> transform applied INSIDE the link
// frame; `joint_origin`/`joint_axis` describe how the link attaches to its parent.
// `joint_index` is Some(i) for a revolute joint driven by `setRobotJoints[i]`, None
// for fixed joints (the body root and feet). Each link owns its own model-matrix
// uniform + bind group so it can be drawn at its own world transform per frame.
struct RobotLink {
    parent: Option<usize>,
    joint_origin: Mat4,
    joint_axis: Vec3,
    joint_index: Option<usize>,
    visual_origin: Mat4,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

// Parsed URDF link/joint metadata, before meshes are fetched. `mesh_part` is the
// base name of the per-link glb (e.g. "thigh_mirror"); None for link with no visual.
struct UrdfLink {
    name: String,
    parent: Option<String>,
    joint_origin: Mat4,
    joint_axis: Vec3,
    revolute: bool,
    joint_name: Option<String>,
    visual_origin: Mat4,
    mesh_part: Option<String>,
}

struct State {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,

    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,

    instance_buffer: wgpu::Buffer,
    instance_capacity: u32,
    instance_count: u32,

    // Overlay (camera-depth) cloud: its own uniform (fixed-colour mode) + instance
    // buffer + cell set, drawn with the SAME pipeline as the map after it. Kept fully
    // separate from `cells` so it never affects framing, the grid, or the colormap.
    overlay_pipeline: wgpu::RenderPipeline,
    overlay_uniform_buffer: wgpu::Buffer,
    overlay_bind_group: wgpu::BindGroup,
    overlay_instance_buffer: wgpu::Buffer,
    overlay_instance_capacity: u32,
    overlay_instance_count: u32,
    overlay_cells: HashMap<(i32, i32, i32), Vec3>,
    overlay_cell_order: VecDeque<(i32, i32, i32)>,

    // Pipeline for colored solid geometry on ModelUniforms (box-marker fallback).
    colored_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,
    // Dedicated robot-mesh pipeline: smooth normal-based Lambert shading.
    robot_pipeline: wgpu::RenderPipeline,
    robot_model_bind_group_layout: wgpu::BindGroupLayout,

    // view_proj-only uniform (identity model) for world-space grid.
    world_uniform_buffer: wgpu::Buffer,
    world_bind_group: wgpu::BindGroup,
    // view_proj + robot model transform.
    robot_uniform_buffer: wgpu::Buffer,
    robot_bind_group: wgpu::BindGroup,

    grid_buffer: wgpu::Buffer,
    grid_vertex_count: u32,
    grid_bounds: Option<GridBounds>,

    robot_buffer: wgpu::Buffer,
    robot_vertex_count: u32,
    robot_model: Mat4,
    robot_visible: bool,

    // Real Go2 body mesh loaded from /assets/go2/go2_full.glb. When present it
    // replaces the box marker; on a fetch/parse failure these stay None and the box
    // marker is used as the fallback. Drawn on the robot pipeline (positions+normals+
    // color) with its own RobotModelUniforms bind group tracking `robot_model`.
    mesh_buffer: Option<wgpu::Buffer>,
    mesh_index_buffer: Option<wgpu::Buffer>,
    mesh_index_count: u32,
    mesh_uniform_buffer: wgpu::Buffer,
    mesh_bind_group: wgpu::BindGroup,

    // Articulated robot: one GPU link per URDF link with a visual mesh, plus the
    // bind-group layout needed to allocate each link's per-frame model uniform.
    // When non-empty this is the primary robot render path and the single-mesh
    // (`mesh_buffer`) / box marker fallbacks are skipped. The kinematic tree is
    // stored as a flat list ordered parents-before-children so a single forward
    // pass computes every world transform.
    robot_links: Vec<RobotLink>,
    // Current 12 leg-joint angles in radians (Go2 order), defaulting to 0.
    joint_angles: [f32; 12],

    depth_view: wgpu::TextureView,

    camera: Camera,
    voxel_size: f32,
    heatmap_origin: Vec3,
    heatmap_range: f32,
    framed: bool,

    // Accumulated occupancy: the union of occupied voxel cells seen so far, keyed by
    // integer cell coordinate (round(world / voxel_size)). The map value is the cell
    // center in world meters, ready to upload as an instance. `cell_order` keeps the
    // FIFO insertion order so we can evict the oldest cells once the cap is hit.
    cells: HashMap<(i32, i32, i32), Vec3>,
    cell_order: VecDeque<(i32, i32, i32)>,
    // Set when the accumulated set changed since the last instance-buffer upload, so
    // the buffer is only re-uploaded on real growth (not every frame).
    cells_dirty: bool,
    // World-space bounds of the accumulated cells, kept incrementally for the grid
    // footprint and occasional re-framing.
    accum_min: Vec3,
    accum_max: Vec3,
    // Latched once after the cap is first reached so the eviction log fires only once.
    capped_logged: bool,

    // Robot world pose from the separate pose topic. `robot_pose_set` gates the
    // marker and the radial colormap origin: before the first pose
    // arrives the marker stays hidden and the colormap falls back to cloud bounds.
    robot_position: Vec3,
    robot_orientation: Quat,
    robot_pose_set: bool,
}

// Geometry of the robot marker, in the marker's local frame (Z-up): a low box
// body plus a triangular nose pointing +X to indicate heading.
const ROBOT_BODY_X: f32 = 0.35;
const ROBOT_BODY_Y: f32 = 0.20;
const ROBOT_BODY_Z: f32 = 0.12;
const ROBOT_NOSE_LEN: f32 = 0.12;
// Near-white so the marker stands out from the height colormap.
const ROBOT_COLOR: [f32; 3] = [0.92, 0.94, 0.97];
const ROBOT_NOSE_COLOR: [f32; 3] = [1.0, 0.85, 0.45];

// URL of the decimated Go2 body glTF binary served by the dashboard. Authored in
// the URDF base_link frame (Z-up, X-forward), real scale in meters, body at the
// origin — the same convention as our odom Z-up frame, so no axis fixup is needed.
// Used ONLY as the single-mesh fallback when the articulated URDF path fails.
const GO2_MESH_URL: &str = "/assets/go2/go2_full.glb";

// Flat light-gray the Go2 body is rendered with on the fallback single-mesh path
// (materials/textures ignored), matching the near-white box marker it replaces.
const GO2_MESH_COLOR: [f32; 3] = [0.82, 0.85, 0.90];

// URDF + per-link mesh asset base for the articulated robot.
const GO2_URDF_URL: &str = "/assets/go2/go2.urdf";
const GO2_ASSET_BASE: &str = "/assets/go2/";

// Names of the 12 revolute leg joints in the order `setRobotJoints` receives them:
// FR(hip,thigh,calf), FL, RR, RL.
const GO2_JOINT_ORDER: [&str; 12] = [
    "FR_hip_joint",
    "FR_thigh_joint",
    "FR_calf_joint",
    "FL_hip_joint",
    "FL_thigh_joint",
    "FL_calf_joint",
    "RR_hip_joint",
    "RR_thigh_joint",
    "RR_calf_joint",
    "RL_hip_joint",
    "RL_thigh_joint",
    "RL_calf_joint",
];

// Go2 color scheme used when a glTF primitive has no material base-color factor:
// dark body, lighter gray legs. Picked by link-name prefix.
const GO2_BODY_FALLBACK_COLOR: [f32; 3] = [0.12, 0.12, 0.13];
const GO2_LEG_FALLBACK_COLOR: [f32; 3] = [0.55, 0.57, 0.60];

impl State {
    fn write_uniforms(&self) {
        let view_proj = self.camera.view_proj();
        let uniforms = Uniforms {
            view_proj: view_proj.to_cols_array_2d(),
            heatmap_origin: self.heatmap_origin.to_array(),
            inv_heatmap_range: 1.0 / self.heatmap_range.max(0.5),
            voxel_size: self.voxel_size * VOXEL_FILL_FACTOR,
            _pad: [0.0; 3],
            overlay: [0.0; 4], // map cloud: radial colormap (mode off)
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

        // Overlay cloud (camera depth): same view, fixed magenta so it stands out
        // against the lidar map's rainbow colormap.
        let overlay_uniforms = Uniforms {
            overlay: [1.0, OVERLAY_COLOR[0], OVERLAY_COLOR[1], OVERLAY_COLOR[2]],
            ..uniforms
        };
        self.queue.write_buffer(
            &self.overlay_uniform_buffer,
            0,
            bytemuck::bytes_of(&overlay_uniforms),
        );

        // World-space colored geometry (grid): identity model.
        let world = ModelUniforms {
            view_proj: view_proj.to_cols_array_2d(),
            model: Mat4::IDENTITY.to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.world_uniform_buffer, 0, bytemuck::bytes_of(&world));

        // Robot marker: view_proj + its world placement.
        let robot = ModelUniforms {
            view_proj: view_proj.to_cols_array_2d(),
            model: self.robot_model.to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.robot_uniform_buffer, 0, bytemuck::bytes_of(&robot));

        // Single-mesh Go2 fallback: same world placement, on the robot pipeline.
        let mesh_u = RobotModelUniforms {
            view_proj: view_proj.to_cols_array_2d(),
            model: self.robot_model.to_cols_array_2d(),
            normal_mat: normal_matrix(&self.robot_model).to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.mesh_uniform_buffer, 0, bytemuck::bytes_of(&mesh_u));

        // Articulated robot: forward kinematics from the body pose down the tree,
        // writing each link's per-frame model uniform (view_proj + world transform).
        self.update_robot_links(&view_proj);
    }

    // Compute every link's world transform via forward kinematics and upload its
    // model uniform. The list is ordered parents-before-children, so each link's
    // parent world transform is already resolved when we reach it. World transform =
    // parent_world * joint_origin * joint_rotation(angle about axis) * visual_origin.
    // The root link (base_link, parent None) uses `robot_model` as its body pose.
    fn update_robot_links(&self, view_proj: &Mat4) {
        if self.robot_links.is_empty() {
            return;
        }
        let vp = view_proj.to_cols_array_2d();
        // Link world transforms WITHOUT the visual origin (children attach to the
        // link frame, not the visual frame), indexed parallel to `robot_links`.
        let mut link_world: Vec<Mat4> = Vec::with_capacity(self.robot_links.len());
        for link in self.robot_links.iter() {
            let parent_world = match link.parent {
                Some(p) => link_world[p],
                None => self.robot_model,
            };
            let joint_rot = match link.joint_index {
                Some(idx) => Mat4::from_axis_angle(link.joint_axis, self.joint_angles[idx]),
                None => Mat4::IDENTITY,
            };
            let world = parent_world * link.joint_origin * joint_rot;
            link_world.push(world);

            let model = world * link.visual_origin;
            let u = RobotModelUniforms {
                view_proj: vp,
                model: model.to_cols_array_2d(),
                normal_mat: normal_matrix(&model).to_cols_array_2d(),
            };
            self.queue
                .write_buffer(&link.uniform_buffer, 0, bytemuck::bytes_of(&u));
        }
    }

    fn render(&self) {
        self.write_uniforms();

        let frame = match self.surface.get_current_texture() {
            Ok(f) => f,
            // Surface lost/outdated (e.g. canvas resize race) — skip this frame;
            // the next configure/resize restores it.
            Err(_) => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("voxel-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                occlusion_query_set: None,
                timestamp_writes: None,
            });

            // Ground grid (wireframe) first so voxels and the robot draw over it.
            if self.grid_vertex_count > 0 {
                pass.set_pipeline(&self.line_pipeline);
                pass.set_bind_group(0, &self.world_bind_group, &[]);
                pass.set_vertex_buffer(0, self.grid_buffer.slice(..));
                pass.draw(0..self.grid_vertex_count, 0..1);
            }

            // Instanced voxel cubes (the lidar/shared map, radial colormap).
            if self.instance_count > 0 {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..CUBE_INDICES.len() as u32, 0, 0..self.instance_count);
            }

            // Overlay cloud (camera depth) on top, same pipeline + fixed-colour bind
            // group, so it overlays the map in a single distinct hue for comparison.
            if self.overlay_instance_count > 0 {
                pass.set_pipeline(&self.overlay_pipeline);
                pass.set_bind_group(0, &self.overlay_bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.overlay_instance_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..CUBE_INDICES.len() as u32, 0, 0..self.overlay_instance_count);
            }

            // Robot on top so it reads as the focal point. Primary path: the
            // articulated per-link robot (each link drawn at its own world transform
            // computed from the live joint angles). Fallbacks, in order: the single
            // pre-assembled Go2 mesh, then the box marker — so the robot is always
            // visible even if the URDF/mesh load failed.
            if self.robot_visible {
                if !self.robot_links.is_empty() {
                    pass.set_pipeline(&self.robot_pipeline);
                    for link in &self.robot_links {
                        if link.index_count == 0 {
                            continue;
                        }
                        pass.set_bind_group(0, &link.bind_group, &[]);
                        pass.set_vertex_buffer(0, link.vertex_buffer.slice(..));
                        pass.set_index_buffer(
                            link.index_buffer.slice(..),
                            wgpu::IndexFormat::Uint32,
                        );
                        pass.draw_indexed(0..link.index_count, 0, 0..1);
                    }
                } else {
                    match (self.mesh_buffer.as_ref(), self.mesh_index_buffer.as_ref()) {
                        (Some(vbuf), Some(ibuf)) if self.mesh_index_count > 0 => {
                            pass.set_pipeline(&self.robot_pipeline);
                            pass.set_bind_group(0, &self.mesh_bind_group, &[]);
                            pass.set_vertex_buffer(0, vbuf.slice(..));
                            pass.set_index_buffer(ibuf.slice(..), wgpu::IndexFormat::Uint32);
                            pass.draw_indexed(0..self.mesh_index_count, 0, 0..1);
                        }
                        _ if self.robot_vertex_count > 0 => {
                            pass.set_pipeline(&self.colored_pipeline);
                            pass.set_bind_group(0, &self.robot_bind_group, &[]);
                            pass.set_vertex_buffer(0, self.robot_buffer.slice(..));
                            pass.draw(0..self.robot_vertex_count, 0..1);
                        }
                        _ => {}
                    }
                }
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
    }

    // Rebuild the wireframe ground grid only when the floor footprint changed
    // materially. Lines lie a hair below min Z to avoid z-fighting with floor
    // voxels.
    fn update_grid(&mut self, min: Vec3, max: Vec3) {
        // Snap the padded footprint outward to whole grid cells.
        let min_x = (min.x - GRID_PADDING) / GRID_CELL_SIZE;
        let max_x = (max.x + GRID_PADDING) / GRID_CELL_SIZE;
        let min_y = (min.y - GRID_PADDING) / GRID_CELL_SIZE;
        let max_y = (max.y + GRID_PADDING) / GRID_CELL_SIZE;
        let min_x = min_x.floor() * GRID_CELL_SIZE;
        let max_x = max_x.ceil() * GRID_CELL_SIZE;
        let min_y = min_y.floor() * GRID_CELL_SIZE;
        let max_y = max_y.ceil() * GRID_CELL_SIZE;
        // Drop the grid a small fraction of a cell below the floor.
        let z = min.z - 0.01;

        let new_bounds = GridBounds {
            min_x,
            min_y,
            max_x,
            max_y,
            z,
        };

        if let Some(prev) = self.grid_bounds {
            let unchanged = (prev.min_x - min_x).abs() < GRID_REBUILD_EPSILON
                && (prev.max_x - max_x).abs() < GRID_REBUILD_EPSILON
                && (prev.min_y - min_y).abs() < GRID_REBUILD_EPSILON
                && (prev.max_y - max_y).abs() < GRID_REBUILD_EPSILON
                && (prev.z - z).abs() < GRID_REBUILD_EPSILON;
            if unchanged {
                return;
            }
        }

        let mut verts: Vec<LineVertex> = Vec::new();
        // Lines parallel to Y (varying X).
        let mut x = min_x;
        while x <= max_x + 1e-4 {
            verts.push(LineVertex {
                position: [x, min_y, z],
                color: GRID_COLOR,
            });
            verts.push(LineVertex {
                position: [x, max_y, z],
                color: GRID_COLOR,
            });
            x += GRID_CELL_SIZE;
        }
        // Lines parallel to X (varying Y).
        let mut y = min_y;
        while y <= max_y + 1e-4 {
            verts.push(LineVertex {
                position: [min_x, y, z],
                color: GRID_COLOR,
            });
            verts.push(LineVertex {
                position: [max_x, y, z],
                color: GRID_COLOR,
            });
            y += GRID_CELL_SIZE;
        }

        // The grid buffer is sized generously once at init; recreate it if a very
        // large footprint ever exceeds the capacity.
        let needed = (verts.len() * std::mem::size_of::<LineVertex>()) as u64;
        if needed > self.grid_buffer.size() {
            self.grid_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("grid-vertices"),
                size: needed.next_power_of_two(),
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        self.queue
            .write_buffer(&self.grid_buffer, 0, bytemuck::cast_slice(&verts));
        self.grid_vertex_count = verts.len() as u32;
        self.grid_bounds = Some(new_bounds);
    }

    // Build the robot marker geometry (triangle list) in its local frame and
    // upload it. The marker is placed in world space via `robot_model`. The body is
    // centered on the local origin in ALL axes (Z spans -hz..+hz), because the pose
    // topic's `z` is the robot BODY CENTER (~0.31 m above the floor) — anchoring the
    // box on local z=0 would lift the whole marker by half its height.
    fn build_robot_geometry() -> Vec<SolidVertex> {
        let hx = ROBOT_BODY_X * 0.5;
        let hy = ROBOT_BODY_Y * 0.5;
        let hz = ROBOT_BODY_Z * 0.5;

        // Box corners (local frame, centered on z = -hz .. +hz).
        let p = |x: f32, y: f32, z: f32| [x, y, z];
        let c = ROBOT_COLOR;
        let corners = [
            p(-hx, -hy, -hz),
            p(hx, -hy, -hz),
            p(hx, hy, -hz),
            p(-hx, hy, -hz),
            p(-hx, -hy, hz),
            p(hx, -hy, hz),
            p(hx, hy, hz),
            p(-hx, hy, hz),
        ];
        // CCW faces (matches the cube pipeline's Ccw + back-cull).
        let faces: [[usize; 6]; 6] = [
            [0, 2, 1, 0, 3, 2], // bottom (-Z) — wound so the outward normal is -Z
            [4, 5, 6, 4, 6, 7], // top (+Z)
            [0, 1, 5, 0, 5, 4], // -Y
            [3, 7, 6, 3, 6, 2], // +Y
            [0, 4, 7, 0, 7, 3], // -X
            [1, 2, 6, 1, 6, 5], // +X
        ];
        let mut verts = Vec::new();
        for f in faces {
            for idx in f {
                verts.push(SolidVertex {
                    position: corners[idx],
                    color: c,
                });
            }
        }

        // Triangular nose pointing +X (heading indicator). The robot's yaw is folded
        // into `robot_model` (translate * quaternion) from the pose topic, so the
        // local nose always points +X here.
        let tip = [hx + ROBOT_NOSE_LEN, 0.0, 0.0];
        let left = [hx, -hy, 0.0];
        let right = [hx, hy, 0.0];
        let nc = ROBOT_NOSE_COLOR;
        // Two-sided so the nose is visible regardless of cull winding.
        for tri in [[tip, left, right], [tip, right, left]] {
            for v in tri {
                verts.push(SolidVertex {
                    position: v,
                    color: nc,
                });
            }
        }
        verts
    }

    // Upload a parsed Go2 body mesh (flat-colored triangle list) as the robot model,
    // replacing the box marker. Reuses the colored pipeline + robot bind group, so the
    // mesh follows the pose via `robot_model` exactly like the box. Indexed to keep the
    // upload compact.
    fn install_robot_mesh(&mut self, vertices: &[RobotVertex], indices: &[u32]) {
        if vertices.is_empty() || indices.is_empty() {
            return;
        }
        let vbuf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("go2-mesh-vertices"),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let ibuf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("go2-mesh-indices"),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            });
        self.mesh_buffer = Some(vbuf);
        self.mesh_index_buffer = Some(ibuf);
        self.mesh_index_count = indices.len() as u32;
    }

    // Build the articulated robot's GPU links from the parsed URDF and a map of
    // already-fetched per-link meshes (link mesh-part -> (vertices, indices)). Links
    // are emitted in topological order (parents before children) so the per-frame
    // forward-kinematics pass can resolve each parent before its child. Only links
    // that both have a visual mesh AND a successfully fetched/parsed glb become a GPU
    // link; structural links (odom/map/imu/feet without mesh) are skipped but their
    // joints are still folded into descendants via the chain walk. Returns true if at
    // least one renderable link was built (so the caller can keep the fallback).
    fn build_robot_links(
        &mut self,
        urdf: &[UrdfLink],
        meshes: &HashMap<String, (Vec<RobotVertex>, Vec<u32>)>,
    ) -> bool {
        // Resolve link name -> URDF index and verify a single root (parent None).
        let mut name_to_idx: HashMap<&str, usize> = HashMap::new();
        for (i, l) in urdf.iter().enumerate() {
            name_to_idx.insert(l.name.as_str(), i);
        }

        // Topological order over the kinematic tree rooted at the parent-less link.
        // The chain is shallow (<= ~5 deep) so a repeated-insertion walk is fine.
        let mut order: Vec<usize> = Vec::with_capacity(urdf.len());
        let mut placed = vec![false; urdf.len()];
        let mut progressed = true;
        while progressed {
            progressed = false;
            for (i, l) in urdf.iter().enumerate() {
                if placed[i] {
                    continue;
                }
                let parent_ready = match &l.parent {
                    None => true,
                    Some(p) => match name_to_idx.get(p.as_str()) {
                        Some(&pi) => placed[pi],
                        // Parent not in the URDF link set: treat as root-attached.
                        None => true,
                    },
                };
                if parent_ready {
                    placed[i] = true;
                    order.push(i);
                    progressed = true;
                }
            }
        }

        // Map a URDF link index to its position in the emitted RobotLink list, so a
        // child can reference its parent by RobotLink index.
        let mut urdf_to_link: HashMap<usize, usize> = HashMap::new();
        let mut links: Vec<RobotLink> = Vec::new();

        for &ui in &order {
            let l = &urdf[ui];
            // Resolve this link's parent in the EMITTED list by walking up the URDF
            // chain until we hit a link that produced a RobotLink (skips structural
            // links like odom/map). Their joint origins are folded in below.
            let mut parent_link: Option<usize> = None;
            let mut acc_origin = l.joint_origin;
            let mut cursor = l.parent.clone();
            loop {
                match cursor {
                    None => break,
                    Some(pname) => match name_to_idx.get(pname.as_str()) {
                        None => break,
                        Some(&pi) => {
                            if let Some(&li) = urdf_to_link.get(&pi) {
                                parent_link = Some(li);
                                break;
                            }
                            // Parent has no GPU link: absorb its joint origin and keep
                            // climbing. A structural parent is always a fixed joint, so
                            // no rotation is lost.
                            acc_origin = urdf[pi].joint_origin * acc_origin;
                            cursor = urdf[pi].parent.clone();
                        }
                    },
                }
            }

            let mesh = match &l.mesh_part {
                Some(part) => meshes.get(part),
                None => None,
            };
            let (verts, idx) = match mesh {
                Some(m) if !m.0.is_empty() && !m.1.is_empty() => m,
                _ => continue,
            };

            let joint_index = l
                .joint_name
                .as_deref()
                .and_then(|jn| GO2_JOINT_ORDER.iter().position(|n| *n == jn))
                .filter(|_| l.revolute);

            let vbuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("go2-link-vertices"),
                    contents: bytemuck::cast_slice(verts),
                    usage: wgpu::BufferUsages::VERTEX,
                });
            let ibuf = self
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("go2-link-indices"),
                    contents: bytemuck::cast_slice(idx),
                    usage: wgpu::BufferUsages::INDEX,
                });
            let ubuf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("go2-link-uniforms"),
                size: std::mem::size_of::<RobotModelUniforms>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("go2-link-bg"),
                layout: &self.robot_model_bind_group_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: ubuf.as_entire_binding(),
                }],
            });

            urdf_to_link.insert(ui, links.len());
            links.push(RobotLink {
                parent: parent_link,
                joint_origin: acc_origin,
                joint_axis: l.joint_axis,
                joint_index,
                visual_origin: l.visual_origin,
                vertex_buffer: vbuf,
                index_buffer: ibuf,
                index_count: idx.len() as u32,
                uniform_buffer: ubuf,
                bind_group: bg,
            });
        }

        if links.is_empty() {
            return false;
        }
        self.robot_links = links;
        true
    }

    // Quantize a world point to its voxel cell coordinate (round to the nearest
    // multiple of the voxel pitch). Two points in the same cell collapse to one key,
    // so re-observing the same surface across frames does not grow the set.
    fn cell_key(&self, p: Vec3) -> (i32, i32, i32) {
        let inv = 1.0 / self.voxel_size;
        (
            (p.x * inv).round() as i32,
            (p.y * inv).round() as i32,
            (p.z * inv).round() as i32,
        )
    }

    // Cell center in world meters from its integer key.
    fn cell_center(&self, key: (i32, i32, i32)) -> Vec3 {
        Vec3::new(
            key.0 as f32 * self.voxel_size,
            key.1 as f32 * self.voxel_size,
            key.2 as f32 * self.voxel_size,
        )
    }

    // Insert one frame's cells into the accumulated union. New cells extend the FIFO
    // order and the world bounds; once the cap is exceeded the oldest cells are
    // evicted. Returns true if the set changed (so the instance buffer needs a
    // re-upload).
    fn accumulate_cells(&mut self, points: &[f32], n: usize) -> bool {
        let mut changed = false;
        for i in 0..n {
            let p = Vec3::new(points[i * 3], points[i * 3 + 1], points[i * 3 + 2]);
            let key = self.cell_key(p);
            let center = self.cell_center(key);
            if self.cells.insert(key, center).is_none() {
                self.cell_order.push_back(key);
                self.accum_min = self.accum_min.min(center);
                self.accum_max = self.accum_max.max(center);
                changed = true;
            }
        }

        // FIFO eviction once over the cap. Removing oldest cells does not shrink the
        // cached bounds (recomputing them every eviction is not worth it); the grid
        // footprint may stay slightly larger than the live set, which is harmless.
        while self.cells.len() > MAX_ACCUMULATED_CELLS {
            if let Some(old) = self.cell_order.pop_front() {
                // Skip stale order entries whose cell was already replaced/removed.
                if self.cells.remove(&old).is_some() {
                    changed = true;
                }
            } else {
                break;
            }
            if !self.capped_logged {
                log::warn!(
                    "voxel accumulation reached cap of {} cells; evicting oldest (FIFO)",
                    MAX_ACCUMULATED_CELLS
                );
                self.capped_logged = true;
            }
        }

        changed
    }

    // Re-upload the instance buffer from the full accumulated set. Grows the buffer
    // if the cell count outpaced the current capacity. Only called when the set
    // actually changed (the dirty flag), so a static scene never re-uploads.
    fn rebuild_instance_buffer(&mut self) {
        let count = self.cells.len();
        if count == 0 {
            self.instance_count = 0;
            return;
        }
        let mut instances: Vec<Instance> = Vec::with_capacity(count);
        for center in self.cells.values() {
            instances.push(Instance {
                translation: center.to_array(),
            });
        }

        if count as u32 > self.instance_capacity {
            let new_cap = (count as u32).next_power_of_two();
            self.instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("voxel-instances"),
                size: (new_cap as u64) * std::mem::size_of::<Instance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instance_capacity = new_cap;
        }

        self.queue
            .write_buffer(&self.instance_buffer, 0, bytemuck::cast_slice(&instances));
        self.instance_count = count as u32;
    }

    // Re-upload the overlay (camera-depth) instance buffer from its cell set. Mirrors
    // `rebuild_instance_buffer` but on the overlay buffers; called on each replace.
    fn rebuild_overlay_buffer(&mut self) {
        let count = self.overlay_cells.len();
        if count == 0 {
            self.overlay_instance_count = 0;
            return;
        }
        let mut instances: Vec<Instance> = Vec::with_capacity(count);
        for center in self.overlay_cells.values() {
            instances.push(Instance {
                translation: center.to_array(),
            });
        }
        if count as u32 > self.overlay_instance_capacity {
            let new_cap = (count as u32).next_power_of_two();
            self.overlay_instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("voxel-overlay-instances"),
                size: (new_cap as u64) * std::mem::size_of::<Instance>() as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.overlay_instance_capacity = new_cap;
        }
        self.queue.write_buffer(
            &self.overlay_instance_buffer,
            0,
            bytemuck::cast_slice(&instances),
        );
        self.overlay_instance_count = count as u32;
    }

    // Recompute the radial colormap origin + adaptive range from the current robot
    // pose (or the accumulated-cloud center when no pose is set yet) and over the
    // accumulated cells. Called both when a new frame arrives and when the pose
    // advances on its own, so colors keep radiating from the live robot position.
    fn refresh_color_field(&mut self) {
        if self.cells.is_empty() || !self.accum_min.is_finite() || !self.accum_max.is_finite() {
            return;
        }
        let center = (self.accum_min + self.accum_max) * 0.5;
        let color_origin = if self.robot_pose_set {
            self.robot_position
        } else {
            center
        };
        self.heatmap_origin = color_origin;
        let mut max_radius = 0.0f32;
        for c in self.cells.values() {
            let dx = c.x - color_origin.x;
            let dy = c.y - color_origin.y;
            let r = (dx * dx + dy * dy).sqrt();
            if r > max_radius {
                max_radius = r;
            }
        }
        self.heatmap_range = max_radius.max(0.3);
    }

    fn make_depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("voxel-depth"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        tex.create_view(&wgpu::TextureViewDescriptor::default())
    }
}

// -----------------------------------------------------------------------------
// Public JS API
// -----------------------------------------------------------------------------

/// Live point-cloud view bound to a single canvas. Owns the wgpu device, the
/// instanced pipeline, the requestAnimationFrame loop and the pointer handlers.
#[wasm_bindgen]
pub struct VoxelView {
    // `Option` so `dispose` can drop the State (device, surface, buffers, depth
    // texture) deterministically instead of waiting for JS GC of the wrapper.
    state: Option<Rc<RefCell<State>>>,
    raf_handle: Rc<RefCell<Option<i32>>>,
    // Kept alive for the lifetime of the view; dropped on `dispose`.
    closures: Vec<Closure<dyn FnMut(web_sys::Event)>>,
    raf_closure: Rc<RefCell<Option<Closure<dyn FnMut()>>>>,
    canvas: web_sys::HtmlCanvasElement,
}

/// Initialize the voxel renderer on `canvas`. Requests a wgpu adapter/device,
/// configures the surface, builds the instanced + grid + robot pipelines, installs
/// orbit/zoom pointer handlers and starts the render loop. `voxelSize` is the cube
/// edge length in meters (the LiDAR resolution, ~0.05).
#[wasm_bindgen(js_name = initVoxelView)]
pub async fn init_voxel_view(
    canvas: web_sys::HtmlCanvasElement,
    voxel_size: f32,
) -> Result<VoxelView, JsValue> {
    #[cfg(feature = "console_error_panic_hook")]
    console_error_panic_hook::set_once();
    #[cfg(feature = "console_log")]
    let _ = console_log::init_with_level(log::Level::Warn);

    let (width, height) = canvas_backing_size(&canvas);

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        // GL backend = WebGL2 in the browser; works without native WebGPU.
        backends: wgpu::Backends::GL | wgpu::Backends::BROWSER_WEBGPU,
        ..Default::default()
    });

    let surface = instance
        .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
        .map_err(|e| JsValue::from_str(&format!("create_surface failed: {e}")))?;

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        })
        .await
        .ok_or_else(|| JsValue::from_str("no compatible GPU adapter (WebGL2/WebGPU)"))?;

    // WebGL has no downlevel storage/compute; request the GL-compatible limits so
    // device creation succeeds on browsers without native WebGPU.
    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("voxel-device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
            },
            None,
        )
        .await
        .map_err(|e| JsValue::from_str(&format!("request_device failed: {e}")))?;

    let caps = surface.get_capabilities(&adapter);
    let surface_format = caps
        .formats
        .iter()
        .copied()
        .find(|f| !f.is_srgb())
        .unwrap_or(caps.formats[0]);

    let config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: surface_format,
        width: width.max(1),
        height: height.max(1),
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: caps.alpha_modes[0],
        view_formats: vec![],
    };
    surface.configure(&device, &config);

    // Pipeline + static geometry.
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("voxel-shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER_SRC.into()),
    });
    let colored_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("colored-shader"),
        source: wgpu::ShaderSource::Wgsl(COLORED_SHADER_SRC.into()),
    });

    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("cube-vertices"),
        contents: bytemuck::cast_slice(&CUBE_VERTICES),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("cube-indices"),
        contents: bytemuck::cast_slice(&CUBE_INDICES),
        usage: wgpu::BufferUsages::INDEX,
    });

    let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("voxel-uniforms"),
        size: std::mem::size_of::<Uniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("voxel-bgl"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("voxel-bg"),
        layout: &bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buffer.as_entire_binding(),
        }],
    });

    // Overlay uniform + bind group: same layout, separate buffer so the overlay
    // (camera-depth) draw can use fixed-colour mode while the map draw stays radial.
    let overlay_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("voxel-overlay-uniforms"),
        size: std::mem::size_of::<Uniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let overlay_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("voxel-overlay-bg"),
        layout: &bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: overlay_uniform_buffer.as_entire_binding(),
        }],
    });

    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("voxel-pl"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("voxel-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            compilation_options: Default::default(),
            buffers: &[
                wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CubeVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3],
                },
                wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![1 => Float32x3],
                },
            ],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            // No culling: the cube index winding was showing inner faces (the
            // outer faces were being culled). Depth-testing both faces shows the
            // correct outer surfaces regardless of winding.
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    // Overlay (camera-depth) pipeline: identical to the map pipeline but with a
    // LESS_EQUAL depth test. The overlay draws AFTER the map; when the camera cloud
    // is correctly calibrated it quantizes to the SAME voxel centers (equal depth),
    // and a strict `Less` test would reject it — hiding the magenta exactly where
    // alignment is good. `LessEqual` lets the overlay win at equal depth so it stays
    // visible on top.
    let overlay_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("voxel-overlay-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: "vs_main",
            compilation_options: Default::default(),
            buffers: &[
                wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CubeVertex>() as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x3],
                },
                wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![1 => Float32x3],
                },
            ],
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: "fs_main",
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::LessEqual,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    // --- Colored line/solid pipelines (grid, robot) ---
    let model_bind_group_layout =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
    let model_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("model-pl"),
        bind_group_layouts: &[&model_bind_group_layout],
        push_constant_ranges: &[],
    });

    let colored_vertex_layout = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<LineVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
    };

    // Lines (grid): no culling.
    let line_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("line-pipeline"),
        layout: Some(&model_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &colored_shader,
            entry_point: "vs_main",
            compilation_options: Default::default(),
            buffers: &[colored_vertex_layout.clone()],
        },
        fragment: Some(wgpu::FragmentState {
            module: &colored_shader,
            entry_point: "fs_main",
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::LineList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            // Lines are overlays/underlays: depth-test so they hide behind solid
            // obstacles, but do NOT write depth, so they never punch holes through
            // voxels drawn later (floor voxels extend below the grid's z).
            depth_write_enabled: false,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    // Solid triangles (robot marker): no culling so the two-sided nose shows.
    let colored_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("solid-pipeline"),
        layout: Some(&model_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &colored_shader,
            entry_point: "vs_main",
            compilation_options: Default::default(),
            buffers: &[colored_vertex_layout],
        },
        fragment: Some(wgpu::FragmentState {
            module: &colored_shader,
            entry_point: "fs_main",
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    // --- Dedicated robot-mesh pipeline (positions + normals + color) ---
    let robot_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("robot-shader"),
        source: wgpu::ShaderSource::Wgsl(ROBOT_SHADER_SRC.into()),
    });
    let robot_model_bind_group_layout =
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("robot-model-bgl"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
    let robot_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("robot-pl"),
        bind_group_layouts: &[&robot_model_bind_group_layout],
        push_constant_ranges: &[],
    });
    let robot_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("robot-pipeline"),
        layout: Some(&robot_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &robot_shader,
            entry_point: "vs_main",
            compilation_options: Default::default(),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<RobotVertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3],
            }],
        },
        fragment: Some(wgpu::FragmentState {
            module: &robot_shader,
            entry_point: "fs_main",
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: surface_format,
                blend: Some(wgpu::BlendState::REPLACE),
                write_mask: wgpu::ColorWrites::ALL,
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::Less,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview: None,
    });

    let mesh_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("go2-mesh-uniforms"),
        size: std::mem::size_of::<RobotModelUniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mesh_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("go2-mesh-bg"),
        layout: &robot_model_bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: mesh_uniform_buffer.as_entire_binding(),
        }],
    });

    let world_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("world-uniforms"),
        size: std::mem::size_of::<ModelUniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let world_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("world-bg"),
        layout: &model_bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: world_uniform_buffer.as_entire_binding(),
        }],
    });
    let robot_uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("robot-uniforms"),
        size: std::mem::size_of::<ModelUniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let robot_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("robot-bg"),
        layout: &model_bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: robot_uniform_buffer.as_entire_binding(),
        }],
    });

    // Robot geometry is static in its local frame; upload once.
    let robot_geometry = State::build_robot_geometry();
    let robot_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("robot-vertices"),
        contents: bytemuck::cast_slice(&robot_geometry),
        usage: wgpu::BufferUsages::VERTEX,
    });
    let robot_vertex_count = robot_geometry.len() as u32;

    // Grid buffer: sized for a generous footprint; grows on demand in update_grid.
    let grid_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("grid-vertices"),
        size: 64 * 1024,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let instance_capacity = 65_536u32;
    let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("voxel-instances"),
        size: (instance_capacity as u64) * std::mem::size_of::<Instance>() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let overlay_instance_capacity = 65_536u32;
    let overlay_instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("voxel-overlay-instances"),
        size: (overlay_instance_capacity as u64) * std::mem::size_of::<Instance>() as u64,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let depth_view = State::make_depth_view(&device, config.width, config.height);

    let mut camera = Camera::new();
    camera.aspect = config.width as f32 / config.height.max(1) as f32;

    let state = Rc::new(RefCell::new(State {
        surface,
        device,
        queue,
        config,
        pipeline,
        vertex_buffer,
        index_buffer,
        uniform_buffer,
        bind_group,
        instance_buffer,
        instance_capacity,
        instance_count: 0,
        overlay_pipeline,
        overlay_uniform_buffer,
        overlay_bind_group,
        overlay_instance_buffer,
        overlay_instance_capacity,
        overlay_instance_count: 0,
        overlay_cells: HashMap::new(),
        overlay_cell_order: VecDeque::new(),
        colored_pipeline,
        line_pipeline,
        robot_pipeline,
        robot_model_bind_group_layout,
        world_uniform_buffer,
        world_bind_group,
        robot_uniform_buffer,
        robot_bind_group,
        grid_buffer,
        grid_vertex_count: 0,
        grid_bounds: None,
        robot_buffer,
        robot_vertex_count,
        robot_model: Mat4::IDENTITY,
        robot_visible: false,
        mesh_buffer: None,
        mesh_index_buffer: None,
        mesh_index_count: 0,
        mesh_uniform_buffer,
        mesh_bind_group,
        robot_links: Vec::new(),
        joint_angles: [0.0; 12],
        depth_view,
        camera,
        voxel_size,
        heatmap_origin: Vec3::ZERO,
        heatmap_range: HEATMAP_RANGE_METERS,
        framed: false,
        cells: HashMap::new(),
        cell_order: VecDeque::new(),
        cells_dirty: false,
        accum_min: Vec3::splat(f32::INFINITY),
        accum_max: Vec3::splat(f32::NEG_INFINITY),
        capped_logged: false,
        robot_position: Vec3::ZERO,
        robot_orientation: Quat::IDENTITY,
        robot_pose_set: false,
    }));

    // Primary path: load the URDF + per-link meshes and build the articulated robot.
    // On ANY failure (network, parse, no usable links) fall back to the single
    // pre-assembled go2_full.glb, and if that also fails, the box marker — so the
    // robot is always visible.
    if !load_articulated_robot(&state).await {
        log::warn!("Go2 articulated load failed; falling back to single go2_full.glb mesh");
        match fetch_bytes(GO2_MESH_URL).await {
            Ok(bytes) => match parse_glb_mesh(&bytes) {
                Ok((verts, idx)) => {
                    state.borrow_mut().install_robot_mesh(&verts, &idx);
                }
                Err(e) => log::warn!("Go2 body mesh parse failed ({e}); using box marker"),
            },
            Err(e) => log::warn!("Go2 body mesh fetch failed ({e:?}); using box marker"),
        }
    }

    let raf_handle: Rc<RefCell<Option<i32>>> = Rc::new(RefCell::new(None));
    let raf_closure: Rc<RefCell<Option<Closure<dyn FnMut()>>>> = Rc::new(RefCell::new(None));

    // Self-rescheduling render loop.
    {
        let state_loop = state.clone();
        let handle_loop = raf_handle.clone();
        let closure_slot = raf_closure.clone();
        let cb = Closure::wrap(Box::new(move || {
            state_loop.borrow().render();
            // Reschedule while the closure is still installed.
            if let Some(cb) = closure_slot.borrow().as_ref() {
                let id = request_animation_frame(cb);
                *handle_loop.borrow_mut() = Some(id);
            }
        }) as Box<dyn FnMut()>);
        *raf_closure.borrow_mut() = Some(cb);
        if let Some(cb) = raf_closure.borrow().as_ref() {
            let id = request_animation_frame(cb);
            *raf_handle.borrow_mut() = Some(id);
        }
    }

    // Pointer + wheel handlers for orbit/zoom.
    let mut closures = Vec::new();
    install_pointer_handlers(&canvas, &state, &mut closures)?;

    Ok(VoxelView {
        state: Some(state),
        raf_handle,
        closures,
        raf_closure,
        canvas,
    })
}

#[wasm_bindgen]
impl VoxelView {
    /// Accumulate a new LiDAR frame into the persistent occupancy map. `points` is
    /// interleaved ODOM-FRAME world XYZ in meters (length = `count` * 3), exactly the
    /// `Float32Array` the dashboard's `decodeLidarFrame(...).points` returns.
    ///
    /// The Go2 voxel_map is already in a fixed odom frame, so accumulation is the
    /// UNION of occupied voxel cells across frames (no per-frame transform): each
    /// point is quantized to its voxel cell and inserted into the persistent set, then
    /// ALL accumulated cells are rendered. The single-frame partial cloud thus grows
    /// into a full map as the robot moves through the fixed grid. The set is capped at
    /// `MAX_ACCUMULATED_CELLS` with FIFO eviction; call `clearAccumulation` to reset.
    ///
    /// On the first non-empty frame the camera auto-frames the accumulated bounds; it
    /// does NOT re-frame on every subsequent growth. The radial colormap, robot marker
    /// is driven by the robot world pose set via `setRobotPose`.
    #[wasm_bindgen(js_name = setPoints)]
    pub fn set_points(&self, points: &[f32], count: u32) {
        let state = match self.state.as_ref() {
            Some(s) => s,
            None => return,
        };
        let mut st = state.borrow_mut();

        let usable = (count as usize).min(points.len() / 3);
        if usable == 0 {
            // An empty frame does not erase the accumulated map; nothing to do.
            return;
        }

        // Merge this frame's cells into the accumulated union.
        if st.accumulate_cells(points, usable) {
            st.cells_dirty = true;
        }

        if st.cells_dirty {
            st.rebuild_instance_buffer();
            st.cells_dirty = false;
        }

        if !st.accum_min.is_finite() || !st.accum_max.is_finite() {
            return;
        }
        let min = st.accum_min;
        let max = st.accum_max;
        let center = (min + max) * 0.5;
        let extent = (max - min).length().max(0.5);

        // Radial-distance colormap origin (robot pose, or cloud center fallback) and
        // adaptive range over the accumulated cells.
        st.refresh_color_field();

        // Ground grid follows the accumulated floor footprint (rebuilt only on
        // material change).
        st.update_grid(min, max);

        // Auto-frame once on the first accumulated cloud; do not re-frame as the map
        // grows so the user's orbit/zoom is preserved.
        if !st.framed {
            st.camera.target = center;
            st.camera.distance = extent * 1.2;
            st.framed = true;
        }
    }

    /// Set the robot's world pose (odom frame, meters + unit quaternion) from the
    /// separate pose topic. Drives the robot marker placement (translate * quaternion),
    /// and the radial colormap origin (color radiates from the robot). Until this is
    /// called the marker stays hidden so it is never drawn at a wrong spot.
    /// `pose.z` already reflects the body center (~0.31 m above the floor).
    #[wasm_bindgen(js_name = setRobotPose)]
    pub fn set_robot_pose(&self, x: f32, y: f32, z: f32, qx: f32, qy: f32, qz: f32, qw: f32) {
        let state = match self.state.as_ref() {
            Some(s) => s,
            None => return,
        };
        let mut st = state.borrow_mut();
        st.robot_position = Vec3::new(x, y, z);
        // Normalize defensively; a zero/denormalized quaternion would otherwise wipe
        // the marker. Fall back to identity if the input is degenerate.
        let q = Quat::from_xyzw(qx, qy, qz, qw);
        st.robot_orientation = if q.length_squared() > 1e-6 {
            q.normalize()
        } else {
            Quat::IDENTITY
        };
        st.robot_model =
            Mat4::from_rotation_translation(st.robot_orientation, st.robot_position);
        st.robot_visible = true;
        st.robot_pose_set = true;

        // Pose-driven visuals must follow the robot even when the pose advances
        // between cloud frames: recompute the colormap origin from the new position.
        st.refresh_color_field();
    }

    /// Set the 12 live leg-joint angles (radians) that articulate the robot. Order is
    /// the Go2 convention: idx 0-2 = FR(hip,thigh,calf), 3-5 = FL, 6-8 = RR, 9-11 = RL,
    /// matching `GO2_JOINT_ORDER`. Fewer than 12 values leaves the remaining joints at
    /// their last value; extra values are ignored. The next rendered frame recomputes
    /// every link's world transform from these angles. No-op until the articulated
    /// robot has loaded (the single-mesh / box fallback has no joints).
    #[wasm_bindgen(js_name = setRobotJoints)]
    pub fn set_robot_joints(&self, joints: &[f32]) {
        let state = match self.state.as_ref() {
            Some(s) => s,
            None => return,
        };
        let mut st = state.borrow_mut();
        let n = joints.len().min(12);
        st.joint_angles[..n].copy_from_slice(&joints[..n]);
    }

    /// Reset the accumulated occupancy map: clears the cell set, the FIFO order and the
    /// rendered instance buffer, and lets the next frame re-frame the camera. The robot
    /// pose and camera orientation are left untouched.
    #[wasm_bindgen(js_name = clearAccumulation)]
    pub fn clear_accumulation(&self) {
        let state = match self.state.as_ref() {
            Some(s) => s,
            None => return,
        };
        let mut st = state.borrow_mut();
        st.cells.clear();
        st.cell_order.clear();
        st.cells_dirty = false;
        st.instance_count = 0;
        st.accum_min = Vec3::splat(f32::INFINITY);
        st.accum_max = Vec3::splat(f32::NEG_INFINITY);
        st.capped_logged = false;
        // Drop the stale ground grid so the previous map's footprint stops rendering;
        // the next non-empty frame rebuilds it for the fresh map.
        st.grid_vertex_count = 0;
        st.grid_bounds = None;
        // Allow the next accumulated cloud to re-frame the camera to the fresh map.
        st.framed = false;
    }

    /// Render the AUTHORITATIVE server scene map: a full REPLACE of the occupied set
    /// with `points` (interleaved world XYZ, length = `count` * 3 — the
    /// `decodeLidarFrame(...).points` of a `scene:<robot_id>` frame). Unlike
    /// `setPoints` (which UNIONs live frames), this drops the current set and renders
    /// exactly the server's deduplicated map, so the server stays the single source of
    /// truth and stale cells never linger. Live `lidar:` frames may still `setPoints`
    /// on top between snapshots for low-latency feel; the next `setMapPoints` reconciles
    /// back to the authoritative map. Camera framing is preserved across replaces (only
    /// the first non-empty map frames the camera) so the 1 Hz snapshot never fights the
    /// user's orbit/zoom.
    #[wasm_bindgen(js_name = setMapPoints)]
    pub fn set_map_points(&self, points: &[f32], count: u32) {
        let state = match self.state.as_ref() {
            Some(s) => s,
            None => return,
        };
        let mut st = state.borrow_mut();

        // Full replace: drop the prior set + bounds, but KEEP `framed` so the camera
        // is not re-framed on every snapshot.
        st.cells.clear();
        st.cell_order.clear();
        st.accum_min = Vec3::splat(f32::INFINITY);
        st.accum_max = Vec3::splat(f32::NEG_INFINITY);
        st.capped_logged = false;

        let usable = (count as usize).min(points.len() / 3);
        if usable == 0 {
            // An empty authoritative map clears the render (server says nothing yet).
            st.instance_count = 0;
            st.cells_dirty = false;
            st.grid_vertex_count = 0;
            st.grid_bounds = None;
            return;
        }

        st.accumulate_cells(points, usable);
        st.rebuild_instance_buffer();
        st.cells_dirty = false;

        if !st.accum_min.is_finite() || !st.accum_max.is_finite() {
            return;
        }
        let min = st.accum_min;
        let max = st.accum_max;
        let center = (min + max) * 0.5;
        let extent = (max - min).length().max(0.5);
        st.refresh_color_field();
        st.update_grid(min, max);
        if !st.framed {
            st.camera.target = center;
            st.camera.distance = extent * 1.2;
            st.framed = true;
        }
    }

    /// Replace the OVERLAY cloud (camera-depth `scene-depth:<id>` snapshot) — a
    /// second cloud rendered in one fixed colour over the lidar map for side-by-side
    /// calibration. Like `setMapPoints` it is authoritative-replace, but it never
    /// touches the camera framing, grid, or colormap (those stay driven by the map).
    /// An empty frame clears the overlay.
    #[wasm_bindgen(js_name = setOverlayPoints)]
    pub fn set_overlay_points(&self, points: &[f32], count: u32) {
        let state = match self.state.as_ref() {
            Some(s) => s,
            None => return,
        };
        let mut st = state.borrow_mut();
        st.overlay_cells.clear();
        st.overlay_cell_order.clear();

        let usable = (count as usize).min(points.len() / 3);
        if usable == 0 {
            st.overlay_instance_count = 0;
            return;
        }
        for i in 0..usable {
            let p = Vec3::new(points[i * 3], points[i * 3 + 1], points[i * 3 + 2]);
            if !p.is_finite() {
                continue;
            }
            let key = st.cell_key(p);
            let center = st.cell_center(key);
            if st.overlay_cells.insert(key, center).is_none() {
                st.overlay_cell_order.push_back(key);
            }
        }
        // FIFO cap, same bound as the map set.
        while st.overlay_cells.len() > MAX_ACCUMULATED_CELLS {
            match st.overlay_cell_order.pop_front() {
                Some(old) => {
                    st.overlay_cells.remove(&old);
                }
                None => break,
            }
        }
        st.rebuild_overlay_buffer();
    }

    /// Reconfigure the surface and depth buffer for a new backing size in
    /// physical pixels. Pass the device-pixel-ratio-scaled canvas dimensions.
    #[wasm_bindgen]
    pub fn resize(&self, width: u32, height: u32) {
        let state = match self.state.as_ref() {
            Some(s) => s,
            None => return,
        };
        let mut st = state.borrow_mut();
        let w = width.max(1);
        let h = height.max(1);
        if st.config.width == w && st.config.height == h {
            return;
        }
        st.config.width = w;
        st.config.height = h;
        st.surface.configure(&st.device, &st.config);
        st.depth_view = State::make_depth_view(&st.device, w, h);
        st.camera.aspect = w as f32 / h as f32;
    }

    /// Stop the render loop and release GPU resources. Safe to call once when
    /// the dashboard leaves the view.
    #[wasm_bindgen]
    pub fn dispose(&mut self) {
        if let Some(id) = self.raf_handle.borrow_mut().take() {
            cancel_animation_frame(id);
        }
        // Dropping the rescheduling closure stops any in-flight reschedule.
        *self.raf_closure.borrow_mut() = None;
        // Remove pointer listeners; dropping the closures frees their JS shims.
        for cb in self.closures.drain(..) {
            let _ = self
                .canvas
                .remove_event_listener_with_callback("pointerdown", cb.as_ref().unchecked_ref());
            let _ = self
                .canvas
                .remove_event_listener_with_callback("pointermove", cb.as_ref().unchecked_ref());
            let _ = self
                .canvas
                .remove_event_listener_with_callback("pointerup", cb.as_ref().unchecked_ref());
            let _ = self
                .canvas
                .remove_event_listener_with_callback("wheel", cb.as_ref().unchecked_ref());
            drop(cb);
        }
        // Drop our owning State handle. With the rAF closure and all pointer
        // closures already dropped above, this releases the last Rc strong
        // reference, so the wgpu device/surface/buffers (including the grid, robot,
        // Go2 mesh and ray pipelines/buffers) are freed now rather than at JS GC of the
        // wrapper.
        self.state = None;
    }
}

// -----------------------------------------------------------------------------
// Pointer / wheel handling
// -----------------------------------------------------------------------------

// Drag state shared between the pointer handlers.
struct DragState {
    active: bool,
    last_x: f32,
    last_y: f32,
}

fn install_pointer_handlers(
    canvas: &web_sys::HtmlCanvasElement,
    state: &Rc<RefCell<State>>,
    closures: &mut Vec<Closure<dyn FnMut(web_sys::Event)>>,
) -> Result<(), JsValue> {
    let drag = Rc::new(RefCell::new(DragState {
        active: false,
        last_x: 0.0,
        last_y: 0.0,
    }));

    // pointerdown
    {
        let drag = drag.clone();
        let cb = Closure::wrap(Box::new(move |ev: web_sys::Event| {
            let pe: web_sys::PointerEvent = ev.unchecked_into();
            let mut d = drag.borrow_mut();
            d.active = true;
            d.last_x = pe.client_x() as f32;
            d.last_y = pe.client_y() as f32;
        }) as Box<dyn FnMut(web_sys::Event)>);
        canvas.add_event_listener_with_callback("pointerdown", cb.as_ref().unchecked_ref())?;
        closures.push(cb);
    }

    // pointermove
    {
        let drag = drag.clone();
        let state = state.clone();
        let cb = Closure::wrap(Box::new(move |ev: web_sys::Event| {
            let pe: web_sys::PointerEvent = ev.unchecked_into();
            let mut d = drag.borrow_mut();
            if !d.active {
                return;
            }
            let x = pe.client_x() as f32;
            let y = pe.client_y() as f32;
            let dx = x - d.last_x;
            let dy = y - d.last_y;
            d.last_x = x;
            d.last_y = y;

            let mut st = state.borrow_mut();
            st.camera.azimuth -= dx * 0.01;
            st.camera.elevation = (st.camera.elevation + dy * 0.01)
                .clamp(-1.5533, 1.5533); // keep just inside ±90° to avoid gimbal flip
        }) as Box<dyn FnMut(web_sys::Event)>);
        canvas.add_event_listener_with_callback("pointermove", cb.as_ref().unchecked_ref())?;
        closures.push(cb);
    }

    // pointerup
    {
        let drag = drag.clone();
        let cb = Closure::wrap(Box::new(move |_ev: web_sys::Event| {
            drag.borrow_mut().active = false;
        }) as Box<dyn FnMut(web_sys::Event)>);
        canvas.add_event_listener_with_callback("pointerup", cb.as_ref().unchecked_ref())?;
        closures.push(cb);
    }

    // wheel (zoom)
    {
        let state = state.clone();
        let cb = Closure::wrap(Box::new(move |ev: web_sys::Event| {
            let we: web_sys::WheelEvent = ev.unchecked_into();
            we.prevent_default();
            let mut st = state.borrow_mut();
            let factor = if we.delta_y() > 0.0 { 1.1 } else { 1.0 / 1.1 };
            st.camera.distance = (st.camera.distance * factor).clamp(0.05, 5000.0);
        }) as Box<dyn FnMut(web_sys::Event)>);
        // passive:false so prevent_default actually suppresses page scroll.
        let opts = web_sys::AddEventListenerOptions::new();
        opts.set_passive(false);
        canvas.add_event_listener_with_callback_and_add_event_listener_options(
            "wheel",
            cb.as_ref().unchecked_ref(),
            &opts,
        )?;
        closures.push(cb);
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// Browser helpers
// -----------------------------------------------------------------------------

fn window() -> web_sys::Window {
    web_sys::window().expect("no global window")
}

// Fetch the Go2 body glb over HTTP and return its raw bytes. Errors (network,
// non-200) propagate so the caller can fall back to the box marker.
async fn fetch_bytes(url: &str) -> Result<Vec<u8>, JsValue> {
    let opts = web_sys::RequestInit::new();
    opts.set_method("GET");
    let request = web_sys::Request::new_with_str_and_init(url, &opts)?;
    let resp_value =
        wasm_bindgen_futures::JsFuture::from(window().fetch_with_request(&request)).await?;
    let resp: web_sys::Response = resp_value.dyn_into()?;
    if !resp.ok() {
        return Err(JsValue::from_str(&format!(
            "fetch {url} -> HTTP {}",
            resp.status()
        )));
    }
    let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer()?).await?;
    let array = js_sys::Uint8Array::new(&buf);
    Ok(array.to_vec())
}

// Parse three space-separated f32s from an attribute string ("x y z"), defaulting
// missing components to 0.
fn parse_xyz(s: &str) -> Vec3 {
    let mut it = s.split_whitespace().map(|t| t.parse::<f32>().unwrap_or(0.0));
    Vec3::new(
        it.next().unwrap_or(0.0),
        it.next().unwrap_or(0.0),
        it.next().unwrap_or(0.0),
    )
}

// URDF <origin> -> homogeneous transform. URDF applies translation then RPY where
// RPY is the fixed-axis convention R = Rz(yaw) * Ry(pitch) * Rx(roll). glam's
// `from_euler(EulerRot::ZYX, yaw, pitch, roll)` produces exactly that rotation.
fn urdf_origin_to_mat(xyz: Vec3, rpy: Vec3) -> Mat4 {
    let rot = Quat::from_euler(glam::EulerRot::ZYX, rpy.z, rpy.y, rpy.x);
    Mat4::from_rotation_translation(rot, xyz)
}

// Read an element's <origin xyz rpy> child into a transform (identity if absent).
fn read_origin(el: roxmltree::Node) -> Mat4 {
    for c in el.children() {
        if c.has_tag_name("origin") {
            let xyz = parse_xyz(c.attribute("xyz").unwrap_or("0 0 0"));
            let rpy = parse_xyz(c.attribute("rpy").unwrap_or("0 0 0"));
            return urdf_origin_to_mat(xyz, rpy);
        }
    }
    Mat4::IDENTITY
}

// Parse the Go2 URDF into a flat list of links carrying their attaching joint's
// origin/axis/type and their visual <origin> + mesh part name. The mesh part is the
// file stem of the `package://.../dae/<part>.dae` reference, mapped 1:1 to
// `/assets/go2/<part>.glb`. Joints connect parent->child; we store each joint on its
// CHILD link so the kinematic tree is link-indexed.
fn parse_urdf(xml: &str) -> Result<Vec<UrdfLink>, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("urdf parse: {e}"))?;
    let root = doc.root_element();

    // First pass: every link with its visual origin + mesh part (if any).
    let mut links: HashMap<String, UrdfLink> = HashMap::new();
    for link in root.children().filter(|n| n.has_tag_name("link")) {
        let name = match link.attribute("name") {
            Some(n) => n.to_string(),
            None => continue,
        };
        let mut visual_origin = Mat4::IDENTITY;
        let mut mesh_part: Option<String> = None;
        if let Some(visual) = link.children().find(|c| c.has_tag_name("visual")) {
            visual_origin = read_origin(visual);
            if let Some(geom) = visual.children().find(|c| c.has_tag_name("geometry")) {
                if let Some(mesh) = geom.children().find(|c| c.has_tag_name("mesh")) {
                    if let Some(fname) = mesh.attribute("filename") {
                        mesh_part = mesh_part_from_filename(fname);
                    }
                }
            }
        }
        links.insert(
            name.clone(),
            UrdfLink {
                name,
                parent: None,
                joint_origin: Mat4::IDENTITY,
                joint_axis: Vec3::X,
                revolute: false,
                joint_name: None,
                visual_origin,
                mesh_part,
            },
        );
    }

    // Second pass: fold each joint onto its child link (parent, origin, axis, type).
    for joint in root.children().filter(|n| n.has_tag_name("joint")) {
        let jtype = joint.attribute("type").unwrap_or("fixed");
        let jname = joint.attribute("name").map(|s| s.to_string());
        let parent = joint
            .children()
            .find(|c| c.has_tag_name("parent"))
            .and_then(|p| p.attribute("link"))
            .map(|s| s.to_string());
        let child = joint
            .children()
            .find(|c| c.has_tag_name("child"))
            .and_then(|c| c.attribute("link"))
            .map(|s| s.to_string());
        let origin = read_origin(joint);
        let axis = joint
            .children()
            .find(|c| c.has_tag_name("axis"))
            .and_then(|a| a.attribute("xyz"))
            .map(parse_xyz)
            .unwrap_or(Vec3::X);

        if let Some(child_name) = child {
            if let Some(cl) = links.get_mut(&child_name) {
                cl.parent = parent;
                cl.joint_origin = origin;
                cl.joint_axis = if axis.length_squared() > 1e-9 {
                    axis.normalize()
                } else {
                    Vec3::X
                };
                cl.revolute = jtype == "revolute" || jtype == "continuous";
                cl.joint_name = jname;
            }
        }
    }

    if links.is_empty() {
        return Err("urdf had no links".to_string());
    }
    Ok(links.into_values().collect())
}

// "package://go2_robot_sdk/dae/thigh_mirror.dae" -> Some("thigh_mirror").
fn mesh_part_from_filename(filename: &str) -> Option<String> {
    let stem = filename.rsplit('/').next().unwrap_or(filename);
    let stem = stem.strip_suffix(".dae").unwrap_or(stem);
    if stem.is_empty() {
        None
    } else {
        Some(stem.to_string())
    }
}

// Default per-link color when a glTF primitive carries no material base-color
// factor: dark for the body (base) link, lighter gray for the legs.
fn fallback_link_color(link_name: &str) -> [f32; 3] {
    if link_name == "base_link" {
        GO2_BODY_FALLBACK_COLOR
    } else {
        GO2_LEG_FALLBACK_COLOR
    }
}

// Parse a per-link glb into a colored triangle mesh. Each primitive is rendered in
// its material's base-color factor when present; otherwise the link's fallback color.
// Node-internal transforms are applied so the mesh sits correctly in the link frame
// (this is the SAME accumulation as the fallback path, but with real material color).
fn parse_glb_mesh_colored(
    bytes: &[u8],
    fallback_color: [f32; 3],
) -> Result<(Vec<RobotVertex>, Vec<u32>), String> {
    let document = gltf::Gltf::from_slice(bytes).map_err(|e| format!("gltf parse: {e}"))?;
    let bin = document.blob.as_deref().unwrap_or(&[]);
    let buffers: Vec<&[u8]> = vec![bin];

    let mut vertices: Vec<RobotVertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for scene in document.scenes() {
        for node in scene.nodes() {
            accumulate_node_colored(
                &node,
                Mat4::IDENTITY,
                &buffers,
                fallback_color,
                &mut vertices,
                &mut indices,
            );
        }
    }
    if vertices.is_empty() || indices.is_empty() {
        return Err("glb contained no triangle geometry".to_string());
    }
    Ok((vertices, indices))
}

// Like `accumulate_node`, but colors each primitive by its material base-color
// factor (falling back to `fallback_color`) instead of a single flat color.
fn accumulate_node_colored(
    node: &gltf::Node,
    parent: Mat4,
    buffers: &[&[u8]],
    fallback_color: [f32; 3],
    vertices: &mut Vec<RobotVertex>,
    indices: &mut Vec<u32>,
) {
    let local = Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent * local;
    // Normals transform by the inverse-transpose of the upper-3×3; for the Go2's
    // rigid node transforms that equals the rotation part, which we apply directly.
    let normal_world = normal_matrix(&world);

    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            // The Go2 meshes carry real per-primitive materials (black body/feet,
            // light gray-blue legs/panels, white accents) — the authentic two-tone
            // look. Use each primitive's assigned material base-color; only fall back
            // to the link scheme when a primitive has no material at all (gltf returns
            // the default white material with index None).
            let mat = prim.material();
            let color = if mat.index().is_some() {
                let c = mat.pbr_metallic_roughness().base_color_factor();
                // Lift very-dark materials off pure black so the Go2's black body
                // stays visible against the dark viewer background (still clearly
                // darker than the light gray-blue panels).
                [c[0].max(0.07), c[1].max(0.07), c[2].max(0.08)]
            } else {
                fallback_color
            };

            let reader = prim.reader(|buffer| buffers.get(buffer.index()).copied());
            let positions: Vec<[f32; 3]> = match reader.read_positions() {
                Some(p) => p.collect(),
                None => continue,
            };
            // World-space positions for this primitive.
            let world_pos: Vec<Vec3> = positions
                .iter()
                .map(|p| world.transform_point3(Vec3::from_array(*p)))
                .collect();
            // The primitive's triangle index list (the synthesized 0..n when the
            // primitive is non-indexed).
            let tri_idx: Vec<u32> = match reader.read_indices() {
                Some(idx) => idx.into_u32().collect(),
                None => (0..world_pos.len() as u32).collect(),
            };

            // Real per-vertex normals from the glTF are the source of smooth shading.
            // When absent we split each triangle into three fresh vertices carrying
            // that face's normal, so the fallback is genuinely FLAT shaded (shared
            // indexed corners would otherwise average into smooth shading).
            match reader.read_normals() {
                Some(norm_it) => {
                    let normals: Vec<[f32; 3]> = norm_it.collect();
                    let start = vertices.len() as u32;
                    for (i, wp) in world_pos.iter().enumerate() {
                        let raw = normals.get(i).copied().unwrap_or([0.0, 0.0, 1.0]);
                        let n = normal_world
                            .transform_vector3(Vec3::from_array(raw))
                            .normalize_or_zero();
                        vertices.push(RobotVertex {
                            position: wp.to_array(),
                            normal: if n.length_squared() > 0.0 {
                                n.to_array()
                            } else {
                                [0.0, 0.0, 1.0]
                            },
                            color,
                        });
                    }
                    for i in &tri_idx {
                        indices.push(start + *i);
                    }
                }
                None => {
                    let mut t = 0;
                    while t + 3 <= tri_idx.len() {
                        let a = tri_idx[t] as usize;
                        let b = tri_idx[t + 1] as usize;
                        let c = tri_idx[t + 2] as usize;
                        let (pa, pb, pc) = (world_pos[a], world_pos[b], world_pos[c]);
                        let fn_ = (pb - pa).cross(pc - pa).normalize_or_zero();
                        let n = if fn_.length_squared() > 0.0 {
                            fn_.to_array()
                        } else {
                            [0.0, 0.0, 1.0]
                        };
                        for p in [pa, pb, pc] {
                            let base = vertices.len() as u32;
                            vertices.push(RobotVertex {
                                position: p.to_array(),
                                normal: n,
                                color,
                            });
                            indices.push(base);
                        }
                        t += 3;
                    }
                }
            }
        }
    }

    for child in node.children() {
        accumulate_node_colored(&child, world, buffers, fallback_color, vertices, indices);
    }
}

// World-space normal matrix for a model transform. Go2 joints/visual origins are
// rigid (rotation + translation, no scale/shear), so the upper-3×3 is orthonormal
// and is itself the correct normal transform; we just zero the translation column.
fn normal_matrix(model: &Mat4) -> Mat4 {
    let mut m = *model;
    m.w_axis = glam::Vec4::new(0.0, 0.0, 0.0, 1.0);
    m.x_axis.w = 0.0;
    m.y_axis.w = 0.0;
    m.z_axis.w = 0.0;
    m
}


// Fetch the URDF + every referenced per-link glb and build the articulated robot
// on `state`. Returns true on success (at least one renderable link installed). On
// any failure it leaves `robot_links` empty so the caller uses the single-mesh /
// box fallback. Each unique mesh part is fetched once and shared across links that
// reference it (e.g. four legs share thigh/calf parts).
async fn load_articulated_robot(state: &Rc<RefCell<State>>) -> bool {
    let xml = match fetch_bytes(GO2_URDF_URL).await {
        Ok(b) => b,
        Err(e) => {
            log::warn!("Go2 URDF fetch failed ({e:?})");
            return false;
        }
    };
    let xml = match std::str::from_utf8(&xml) {
        Ok(s) => s,
        Err(_) => {
            log::warn!("Go2 URDF is not valid UTF-8");
            return false;
        }
    };
    let urdf = match parse_urdf(xml) {
        Ok(u) => u,
        Err(e) => {
            log::warn!("Go2 URDF parse failed ({e})");
            return false;
        }
    };

    // Collect the unique mesh parts and the fallback color to use for each (decided
    // by the FIRST link that references the part — body vs leg).
    let mut part_color: HashMap<String, [f32; 3]> = HashMap::new();
    for l in &urdf {
        if let Some(part) = &l.mesh_part {
            part_color
                .entry(part.clone())
                .or_insert_with(|| fallback_link_color(&l.name));
        }
    }

    // Fetch + parse each unique per-link mesh once. ALL referenced parts must load:
    // a partial set would render a robot with missing limbs and, worse, would drop a
    // revolute joint whose link mesh is missing (its descendants would then pose
    // incorrectly). On any failure we abort the articulated path so the caller uses
    // the single go2_full.glb fallback instead.
    let mut meshes: HashMap<String, (Vec<RobotVertex>, Vec<u32>)> = HashMap::new();
    for (part, color) in &part_color {
        let url = format!("{GO2_ASSET_BASE}{part}.glb");
        let bytes = match fetch_bytes(&url).await {
            Ok(b) => b,
            Err(e) => {
                log::warn!("Go2 link mesh fetch failed for {part} ({e:?})");
                return false;
            }
        };
        match parse_glb_mesh_colored(&bytes, *color) {
            Ok(geo) => {
                meshes.insert(part.clone(), geo);
            }
            Err(e) => {
                log::warn!("Go2 link mesh parse failed for {part} ({e})");
                return false;
            }
        }
    }

    if meshes.is_empty() {
        return false;
    }

    state.borrow_mut().build_robot_links(&urdf, &meshes)
}

// Parse a binary glTF (.glb) byte buffer into a single merged flat-colored triangle
// mesh: every primitive of every mesh is appended (positions transformed by its
// node's world transform), indices are offset and concatenated. Materials and
// textures are intentionally ignored — the body renders flat. Returns the merged
// (vertices, indices) or an error if the file has no usable triangle geometry.
fn parse_glb_mesh(bytes: &[u8]) -> Result<(Vec<RobotVertex>, Vec<u32>), String> {
    // Gltf::from_slice parses the glb container and exposes its binary chunk as
    // `blob`; that single embedded buffer holds all buffer data for a self-contained
    // file, so map it to the one internal buffer the primitive reader expects.
    let document = gltf::Gltf::from_slice(bytes).map_err(|e| format!("gltf parse: {e}"))?;
    let bin = document.blob.as_deref().unwrap_or(&[]);
    let buffers: Vec<&[u8]> = vec![bin];

    let mut vertices: Vec<RobotVertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Walk every node in every scene and accumulate world-space geometry (with real
    // normals) so a model built from several nodes/primitives merges into one mesh.
    // The single-mesh fallback uses one flat body color for all primitives.
    for scene in document.scenes() {
        for node in scene.nodes() {
            accumulate_node_colored(
                &node,
                Mat4::IDENTITY,
                &buffers,
                GO2_MESH_COLOR,
                &mut vertices,
                &mut indices,
            );
        }
    }

    if vertices.is_empty() || indices.is_empty() {
        return Err("glb contained no triangle geometry".to_string());
    }
    Ok((vertices, indices))
}

fn request_animation_frame(cb: &Closure<dyn FnMut()>) -> i32 {
    window()
        .request_animation_frame(cb.as_ref().unchecked_ref())
        .expect("requestAnimationFrame failed")
}

fn cancel_animation_frame(id: i32) {
    let _ = window().cancel_animation_frame(id);
}

// Backing-store size of the canvas in physical pixels (already DPR-scaled by the
// caller via the canvas width/height attributes).
fn canvas_backing_size(canvas: &web_sys::HtmlCanvasElement) -> (u32, u32) {
    (canvas.width().max(1), canvas.height().max(1))
}
