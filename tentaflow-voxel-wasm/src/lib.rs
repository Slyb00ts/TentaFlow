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

const SHADER_SRC: &str = r#"
struct Uniforms {
    view_proj: mat4x4<f32>,
    heatmap_origin: vec3<f32>,
    inv_heatmap_range: f32,
    voxel_size: f32,
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

    // Color by HORIZONTAL RADIAL DISTANCE from the cloud center (X-Y only, Z
    // ignored). heatmap_origin carries the cloud center; inv_heatmap_range =
    // 1 / max horizontal radius so green reaches the outer edge.
    let dxy = in.translation.xy - u.heatmap_origin.xy;
    let t = length(dxy) * u.inv_heatmap_range;
    out.color = radialcolor(t);
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

// Shared shader for per-vertex-colored line and solid geometry (grid, robot
// marker). The robot draw applies a model matrix; grid bind an identity model.
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
    let world = u.model * vec4<f32>(in.position, 1.0);
    out.clip_position = u.view_proj * world;
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
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

    // Shared pipeline for colored line/solid geometry (grid, robot).
    colored_pipeline: wgpu::RenderPipeline,
    line_pipeline: wgpu::RenderPipeline,

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

    // Real Go2 body mesh loaded from /assets/go2/base.glb at init. When present it
    // replaces the box marker; on a fetch/parse failure these stay None and the box
    // marker is used as the fallback. The mesh reuses the robot bind group (and thus
    // `robot_model`) so it tracks the pose exactly like the box did.
    mesh_buffer: Option<wgpu::Buffer>,
    mesh_index_buffer: Option<wgpu::Buffer>,
    mesh_index_count: u32,


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
const GO2_MESH_URL: &str = "/assets/go2/go2_full.glb";

// Flat light-gray the Go2 body is rendered with (materials/textures are ignored),
// matching the near-white box marker it replaces.
const GO2_MESH_COLOR: [f32; 3] = [0.82, 0.85, 0.90];

impl State {
    fn write_uniforms(&self) {
        let view_proj = self.camera.view_proj();
        let uniforms = Uniforms {
            view_proj: view_proj.to_cols_array_2d(),
            heatmap_origin: self.heatmap_origin.to_array(),
            inv_heatmap_range: 1.0 / self.heatmap_range.max(0.5),
            voxel_size: self.voxel_size * VOXEL_FILL_FACTOR,
            _pad: [0.0; 3],
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

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

            // Instanced voxel cubes.
            if self.instance_count > 0 {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..CUBE_INDICES.len() as u32, 0, 0..self.instance_count);
            }

            // Robot on top so it reads as the focal point. Prefer the real Go2 body
            // mesh (loaded from base.glb); fall back to the box marker if the mesh
            // failed to load, so the robot is always visible. Both reuse the robot bind
            // group, so they track the pose identically.
            if self.robot_visible {
                match (self.mesh_buffer.as_ref(), self.mesh_index_buffer.as_ref()) {
                    (Some(vbuf), Some(ibuf)) if self.mesh_index_count > 0 => {
                        pass.set_pipeline(&self.colored_pipeline);
                        pass.set_bind_group(0, &self.robot_bind_group, &[]);
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
    fn install_robot_mesh(&mut self, vertices: &[SolidVertex], indices: &[u32]) {
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
            cull_mode: Some(wgpu::Face::Back),
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
        colored_pipeline,
        line_pipeline,
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

    // Load the real Go2 body mesh. On any failure (network, parse, no geometry) keep
    // the box marker as the fallback so the robot is always visible, and warn.
    match fetch_bytes(GO2_MESH_URL).await {
        Ok(bytes) => match parse_glb_mesh(&bytes) {
            Ok((verts, idx)) => {
                state.borrow_mut().install_robot_mesh(&verts, &idx);
            }
            Err(e) => log::warn!("Go2 body mesh parse failed ({e}); using box marker"),
        },
        Err(e) => log::warn!(
            "Go2 body mesh fetch failed ({e:?}); using box marker"
        ),
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

// Parse a binary glTF (.glb) byte buffer into a single merged flat-colored triangle
// mesh: every primitive of every mesh is appended (positions transformed by its
// node's world transform), indices are offset and concatenated. Materials and
// textures are intentionally ignored — the body renders flat. Returns the merged
// (vertices, indices) or an error if the file has no usable triangle geometry.
fn parse_glb_mesh(bytes: &[u8]) -> Result<(Vec<SolidVertex>, Vec<u32>), String> {
    // Gltf::from_slice parses the glb container and exposes its binary chunk as
    // `blob`; that single embedded buffer holds all buffer data for a self-contained
    // file, so map it to the one internal buffer the primitive reader expects.
    let document = gltf::Gltf::from_slice(bytes).map_err(|e| format!("gltf parse: {e}"))?;
    let bin = document.blob.as_deref().unwrap_or(&[]);
    let buffers: Vec<&[u8]> = vec![bin];

    let mut vertices: Vec<SolidVertex> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();

    // Walk every node in every scene and accumulate world-space geometry so a model
    // built from several nodes/primitives merges into one mesh.
    for scene in document.scenes() {
        for node in scene.nodes() {
            accumulate_node(&node, Mat4::IDENTITY, &buffers, &mut vertices, &mut indices);
        }
    }

    if vertices.is_empty() || indices.is_empty() {
        return Err("glb contained no triangle geometry".to_string());
    }
    Ok((vertices, indices))
}

// Recursively append a node's mesh primitives (and its children) to the merged
// vertex/index buffers, applying the cumulative world transform.
fn accumulate_node(
    node: &gltf::Node,
    parent: Mat4,
    buffers: &[&[u8]],
    vertices: &mut Vec<SolidVertex>,
    indices: &mut Vec<u32>,
) {
    let local = Mat4::from_cols_array_2d(&node.transform().matrix());
    let world = parent * local;

    if let Some(mesh) = node.mesh() {
        for prim in mesh.primitives() {
            // Only triangle lists carry renderable body geometry here.
            if prim.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            let reader = prim.reader(|buffer| buffers.get(buffer.index()).copied());
            let positions = match reader.read_positions() {
                Some(p) => p,
                None => continue,
            };
            let base = vertices.len() as u32;
            for p in positions {
                let wp = world.transform_point3(Vec3::from_array(p));
                vertices.push(SolidVertex {
                    position: wp.to_array(),
                    color: GO2_MESH_COLOR,
                });
            }
            match reader.read_indices() {
                Some(idx) => {
                    for i in idx.into_u32() {
                        indices.push(base + i);
                    }
                }
                // Non-indexed primitive: emit a sequential index per appended vertex.
                None => {
                    let added = vertices.len() as u32 - base;
                    for i in 0..added {
                        indices.push(base + i);
                    }
                }
            }
        }
    }

    for child in node.children() {
        accumulate_node(&child, world, buffers, vertices, indices);
    }
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
