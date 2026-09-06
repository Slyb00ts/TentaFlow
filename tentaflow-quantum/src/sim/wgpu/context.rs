// ===== File: sim/wgpu/context.rs — adapter, device and the compiled kernel set =====

use std::sync::OnceLock;

use crate::error::{Error, Result};

/// Bytes of the `Params` uniform struct in `kernels.wgsl`; the slot stride is
/// this rounded up to the adapter's dynamic-offset alignment.
pub(crate) const PARAM_BYTES: u64 = 160;

/// Storage buffers one dispatch may bind at once — the four corners of a
/// two-qubit gate whose both operands are shard bits.
const MAX_STORAGE_BINDINGS: u32 = 4;

/// What the picked adapter is, as the UI and `Simulator::describe` show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterReport {
    pub name: String,
    pub backend: String,
    pub device_type: String,
    pub driver: String,
}

pub(crate) struct Layouts {
    /// uniform + one state shard
    pub one: wgpu::BindGroupLayout,
    /// uniform + two state shards
    pub two: wgpu::BindGroupLayout,
    /// uniform + four state shards
    pub four: wgpu::BindGroupLayout,
    /// uniform + one state shard + the f32 partial-sum buffer
    pub reduce: wgpu::BindGroupLayout,
    /// uniform + one state shard + the u32 result buffer + the draw buffer
    pub sample: wgpu::BindGroupLayout,
}

pub(crate) struct Pipelines {
    pub gate1_local: wgpu::ComputePipeline,
    pub gate1_split: wgpu::ComputePipeline,
    pub gate2_local: wgpu::ComputePipeline,
    pub gate2_split_high: wgpu::ComputePipeline,
    pub gate2_split_both: wgpu::ComputePipeline,
    pub global_phase: wgpu::ComputePipeline,
    pub collapse: wgpu::ComputePipeline,
    pub reduce: wgpu::ComputePipeline,
    pub block_sums: wgpu::ComputePipeline,
    pub sample_search: wgpu::ComputePipeline,
}

/// Process-wide GPU context. Opening an adapter costs tens of milliseconds and
/// compiling the kernels costs more, while a session steps thousands of
/// circuits, so both happen once and every backend instance borrows them.
pub struct Gpu {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) limits: wgpu::Limits,
    pub(crate) layouts: Layouts,
    pub(crate) pipelines: Pipelines,
    pub(crate) param_stride: u64,
    report: AdapterReport,
}

impl Gpu {
    pub fn report(&self) -> &AdapterReport {
        &self.report
    }

    /// Largest state shard one storage binding can hold, in amplitudes.
    pub(crate) fn max_shard_amplitudes(&self) -> u64 {
        let bytes = self
            .limits
            .max_storage_buffer_binding_size
            .min(self.limits.max_buffer_size);
        bytes / 8
    }

    pub(crate) fn max_workgroups(&self) -> u32 {
        self.limits.max_compute_workgroups_per_dimension
    }
}

static GPU: OnceLock<std::result::Result<Gpu, String>> = OnceLock::new();

/// The shared context, opened on first use. A machine without a usable adapter
/// reports it once and keeps reporting the same reason.
pub fn gpu() -> Result<&'static Gpu> {
    match GPU.get_or_init(open) {
        Ok(gpu) => Ok(gpu),
        Err(reason) => Err(Error::DeviceUnavailable {
            device: "wgpu".to_string(),
            reason: reason.clone(),
        }),
    }
}

/// Adapter description without allocating a state; the probe the tests and the
/// UI use to decide whether the GPU target exists at all.
pub fn adapter_report() -> Result<&'static AdapterReport> {
    gpu().map(|gpu| gpu.report())
}

fn open() -> std::result::Result<Gpu, String> {
    // Vulkan on Linux and Windows, Metal on Apple, DX12 where Vulkan is absent.
    // The browser tier is a separate build and is not selected here.
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN | wgpu::Backends::METAL | wgpu::Backends::DX12,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: None,
    }))
    .map_err(|error| format!("no Vulkan, Metal or DX12 adapter answered: {error}"))?;

    let info = adapter.get_info();
    let limits = adapter.limits();
    if limits.max_storage_buffers_per_shader_stage < MAX_STORAGE_BINDINGS {
        return Err(format!(
            "adapter `{}` binds only {} storage buffers per stage, the two-qubit split kernel needs {}",
            info.name, limits.max_storage_buffers_per_shader_stage, MAX_STORAGE_BINDINGS
        ));
    }

    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("tentaflow-quantum"),
        required_features: wgpu::Features::empty(),
        required_limits: limits.clone(),
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .map_err(|error| format!("adapter `{}` refused a compute device: {error}", info.name))?;

    // A validation error inside a kernel would otherwise surface as a silently
    // wrong state; turn it into a panic that names the shader instead.
    device.on_uncaptured_error(std::sync::Arc::new(|error| {
        panic!("wgpu validation error in the state-vector kernels: {error}");
    }));

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("statevector-kernels"),
        source: wgpu::ShaderSource::Wgsl(include_str!("kernels.wgsl").into()),
    });

    let layouts = Layouts {
        one: bind_group_layout(&device, "one", &[1]),
        two: bind_group_layout(&device, "two", &[1, 2]),
        four: bind_group_layout(&device, "four", &[1, 2, 3, 4]),
        reduce: bind_group_layout(&device, "reduce", &[1, 5]),
        sample: bind_group_layout(&device, "sample", &[1, 6, 7]),
    };

    let pipelines = Pipelines {
        gate1_local: pipeline(&device, &module, &layouts.one, "gate1_local"),
        gate1_split: pipeline(&device, &module, &layouts.two, "gate1_split"),
        gate2_local: pipeline(&device, &module, &layouts.one, "gate2_local"),
        gate2_split_high: pipeline(&device, &module, &layouts.two, "gate2_split_high"),
        gate2_split_both: pipeline(&device, &module, &layouts.four, "gate2_split_both"),
        global_phase: pipeline(&device, &module, &layouts.one, "global_phase"),
        collapse: pipeline(&device, &module, &layouts.one, "collapse"),
        reduce: pipeline(&device, &module, &layouts.reduce, "reduce"),
        block_sums: pipeline(&device, &module, &layouts.reduce, "block_sums"),
        sample_search: pipeline(&device, &module, &layouts.sample, "sample_search"),
    };

    let alignment = limits.min_uniform_buffer_offset_alignment as u64;
    let param_stride = PARAM_BYTES.div_ceil(alignment) * alignment;

    Ok(Gpu {
        report: AdapterReport {
            name: info.name.clone(),
            backend: info.backend.to_string(),
            device_type: format!("{:?}", info.device_type),
            driver: if info.driver_info.is_empty() {
                info.driver.clone()
            } else {
                format!("{} {}", info.driver, info.driver_info)
            },
        },
        device,
        queue,
        limits,
        layouts,
        pipelines,
        param_stride,
    })
}

fn bind_group_layout(device: &wgpu::Device, label: &str, storage: &[u32]) -> wgpu::BindGroupLayout {
    let mut entries = vec![wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            // One uniform buffer holds every dispatch's parameters and each
            // dispatch picks its slot, so a batch of gates is one write and one
            // command buffer instead of a buffer per gate.
            has_dynamic_offset: true,
            min_binding_size: std::num::NonZeroU64::new(PARAM_BYTES),
        },
        count: None,
    }];
    entries.extend(storage.iter().map(|binding| wgpu::BindGroupLayoutEntry {
        binding: *binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: false },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }));
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &entries,
    })
}

fn pipeline(
    device: &wgpu::Device,
    module: &wgpu::ShaderModule,
    layout: &wgpu::BindGroupLayout,
    entry_point: &str,
) -> wgpu::ComputePipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(entry_point),
        bind_group_layouts: &[Some(layout)],
        immediate_size: 0,
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(entry_point),
        layout: Some(&layout),
        module,
        entry_point: Some(entry_point),
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    })
}
