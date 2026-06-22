// =============================================================================
// File: lib.rs — browser WebGPU/WebGL point-cloud (voxel) renderer
// Live 3D view of a robot's LiDAR cloud (~47k points/frame @ ~7.5 fps) drawn as
// instanced voxel cubes, depth-heatmap colored, with mouse orbit + zoom.
// =============================================================================

use std::cell::RefCell;
use std::rc::Rc;

use glam::{Mat4, Vec3};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wgpu::util::DeviceExt;

// Clear color tuned for a dark dashboard (~#0b0f17).
const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.043,
    g: 0.059,
    b: 0.090,
    a: 1.0,
};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

// Initial heatmap range in meters until the first cloud sets an adaptive range
// from its actual radius. The palette maps depth (distance from the cloud center,
// i.e. the robot) near = red → far = blue; points beyond clamp to the blue end.
const HEATMAP_RANGE_METERS: f32 = 8.0;

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
struct Uniforms {
    view_proj: [[f32; 4]; 4],
    // Reference corner the heatmap distance is measured from + inverse range.
    heatmap_origin: [f32; 3],
    inv_heatmap_range: f32,
    // Cube edge length in meters.
    voxel_size: f32,
    _pad: [f32; 3],
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
};

// Map a normalized depth [0,1] to a near=red -> mid=green/yellow -> far=blue ramp.
fn heatmap(t: f32) -> vec3<f32> {
    let c = clamp(t, 0.0, 1.0);
    let r = clamp(1.5 - abs(c - 0.0) * 3.0, 0.0, 1.0);
    let g = clamp(1.5 - abs(c - 0.5) * 3.0, 0.0, 1.0);
    let b = clamp(1.5 - abs(c - 1.0) * 3.0, 0.0, 1.0);
    return vec3<f32>(r, g, b);
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    let world = in.translation + in.position * u.voxel_size;
    out.clip_position = u.view_proj * vec4<f32>(world, 1.0);

    let dist = length(in.translation - u.heatmap_origin);
    let t = dist * u.inv_heatmap_range;
    out.color = heatmap(t);
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
            azimuth: 0.7,
            elevation: 0.5,
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

    depth_view: wgpu::TextureView,

    camera: Camera,
    voxel_size: f32,
    heatmap_origin: Vec3,
    heatmap_range: f32,
    framed: bool,
}

impl State {
    fn write_uniforms(&self) {
        let uniforms = Uniforms {
            view_proj: self.camera.view_proj().to_cols_array_2d(),
            heatmap_origin: self.heatmap_origin.to_array(),
            inv_heatmap_range: 1.0 / self.heatmap_range.max(0.5),
            voxel_size: self.voxel_size,
            _pad: [0.0; 3],
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
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

            if self.instance_count > 0 {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
                pass.set_vertex_buffer(1, self.instance_buffer.slice(..));
                pass.set_index_buffer(self.index_buffer.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..CUBE_INDICES.len() as u32, 0, 0..self.instance_count);
            }
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
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
/// configures the surface, builds the instanced pipeline, installs orbit/zoom
/// pointer handlers and starts the render loop. `voxelSize` is the cube edge
/// length in meters (the LiDAR resolution, ~0.05).
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
        depth_view,
        camera,
        voxel_size,
        heatmap_origin: Vec3::ZERO,
        heatmap_range: HEATMAP_RANGE_METERS,
        framed: false,
    }));

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
    /// Upload a new point cloud. `points` is interleaved world-space XYZ
    /// (length = `count` * 3), exactly the `Float32Array` that the dashboard's
    /// `decodeLidarFrame(...).points` returns. On the first non-empty cloud the
    /// camera auto-frames the cloud bounds.
    #[wasm_bindgen(js_name = setPoints)]
    pub fn set_points(&self, points: &[f32], count: u32) {
        let state = match self.state.as_ref() {
            Some(s) => s,
            None => return,
        };
        let mut st = state.borrow_mut();

        let usable = (count as usize).min(points.len() / 3);
        if usable == 0 {
            st.instance_count = 0;
            return;
        }

        let n = usable.min(st.instance_capacity as usize);

        // Compute bounds for auto-framing and the heatmap reference corner.
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for i in 0..n {
            let p = Vec3::new(points[i * 3], points[i * 3 + 1], points[i * 3 + 2]);
            min = min.min(p);
            max = max.max(p);
        }

        // The instance buffer is laid out identically to the incoming XYZ
        // triples, so the leading `n*3` floats can be uploaded directly.
        let bytes = bytemuck::cast_slice(&points[..n * 3]);
        st.queue.write_buffer(&st.instance_buffer, 0, bytes);
        st.instance_count = n as u32;

        if min.is_finite() && max.is_finite() {
            let center = (min + max) * 0.5;
            let extent = (max - min).length().max(0.5);
            // Depth heatmap measured from the cloud CENTER (≈ the robot/sensor):
            // near the robot = red, far = blue. Range tracks the cloud radius so
            // the full palette is used regardless of room size.
            st.heatmap_origin = center;
            st.heatmap_range = (extent * 0.5).max(0.5);
            if !st.framed {
                st.camera.target = center;
                st.camera.distance = extent * 1.2;
                st.framed = true;
            }
        }
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
        // reference, so the wgpu device/surface/buffers are freed now rather
        // than at JS GC of the wrapper.
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
