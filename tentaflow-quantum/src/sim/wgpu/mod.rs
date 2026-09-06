// ===== File: sim/wgpu/mod.rs — GPU state-vector backend on Vulkan / Metal / DX12 =====
//
// Plan 6.3: the same IR, the state as `vec2<f32>` in storage buffers, one WGSL
// kernel per gate class, single-qubit gates already fused by the scheduler, and
// shots answered from a prefix reduction on the device instead of 2^n
// probabilities on the host. Plan 18.11 fixes the precision at `complex64`;
// there is no f64 in WGSL and `SHADER_F64` was rejected, so this backend is
// single precision and says so.

mod context;
mod shards;

use std::num::NonZeroU64;

use num_complex::Complex64;

pub use context::{adapter_report, AdapterReport};
pub use shards::{ShardLayout, MAX_ADDRESSABLE_QUBITS, MAX_SHARDS};

use super::cpu::swap_basis_middle;
use super::{Backend, GateOp, Precision};
use crate::error::{Error, Result};
use context::{Gpu, PARAM_BYTES};

/// Threads per workgroup in the gate kernels; must equal `GATE_WG` in the WGSL.
const GATE_WG: u32 = 64;
/// Threads per workgroup in the reductions; must equal `REDUCE_WG` in the WGSL.
const REDUCE_WG: u32 = 256;

/// Uniform slots one submission can address. A batch of gates writes one slot
/// each and is split at this many; every other kernel needs at most one slot
/// per shard.
const PARAM_SLOTS: usize = 256;

/// Partial sums one shard produces in a reduction. The host adds them in f64,
/// so this only trades read-back size against the length of the f32 chain each
/// thread accumulates.
const REDUCE_GROUPS_CAP: u32 = 1024;

/// Leaves of the sampling prefix sum. The host scans them, so the read-back is
/// bounded no matter how wide the register is.
const MAX_BLOCKS: usize = 1 << 18;

/// Amplitudes moved per read-back or upload round trip (16 MiB), so a 2 GiB
/// state never needs a 2 GiB staging buffer.
const CHUNK_AMPLITUDES: usize = 1 << 21;

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuParams {
    a: u32,
    b: u32,
    count: u32,
    flag: u32,
    scale: f32,
    base: u32,
    origin: u32,
    pad: u32,
    m: [[f32; 4]; 8],
}

impl GpuParams {
    fn matrix(mut self, entries: &[Complex64]) -> GpuParams {
        for (index, z) in entries.iter().enumerate() {
            self.m[index >> 1][(index & 1) * 2] = z.re as f32;
            self.m[index >> 1][(index & 1) * 2 + 1] = z.im as f32;
        }
        self
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuDraw {
    block: u32,
    residual: f32,
}

/// One kernel launch: which pipeline, which buffers, and which uniform slot.
struct Dispatch {
    pipeline: &'static wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
    groups: u32,
    slot: u32,
}

pub struct WgpuBackend {
    gpu: &'static Gpu,
    layout: ShardLayout,
    shards: Vec<wgpu::Buffer>,
    params: wgpu::Buffer,
    /// f32 partials of every reduction and every sampling block.
    sums: wgpu::Buffer,
    sums_read: wgpu::Buffer,
    staging: wgpu::Buffer,
    reduce_groups: u32,
    block_len: usize,
    blocks_per_shard: usize,
    chunk_amplitudes: usize,
}

impl WgpuBackend {
    /// Allocate a register on the GPU, splitting it across as few storage
    /// buffers as the adapter's binding ceiling allows.
    pub fn new(num_qubits: usize) -> Result<WgpuBackend> {
        let gpu = context::gpu()?;
        WgpuBackend::build(gpu, num_qubits, gpu.max_shard_amplitudes())
    }

    /// Same, with the shard ceiling forced. The sharded kernels are otherwise
    /// only reachable on a register too large to run in a test, so the split is
    /// a parameter rather than a constant.
    pub fn with_shard_limit(num_qubits: usize, max_shard_amplitudes: u64) -> Result<WgpuBackend> {
        let gpu = context::gpu()?;
        let ceiling = max_shard_amplitudes.min(gpu.max_shard_amplitudes());
        WgpuBackend::build(gpu, num_qubits, ceiling)
    }

    fn build(
        gpu: &'static Gpu,
        num_qubits: usize,
        max_shard_amplitudes: u64,
    ) -> Result<WgpuBackend> {
        let layout = ShardLayout::plan(num_qubits, max_shard_amplitudes)?;
        let shard_len = layout.shard_len();
        let shard_bytes = (shard_len * 8) as u64;

        // A register that fits the binding limit can still not fit the card.
        // Without a scope the allocation failure reaches the uncaptured-error
        // handler and aborts the process; with one it is a refusal the caller
        // can answer by moving the run to the CPU.
        let out_of_memory = gpu.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);

        let shards: Vec<wgpu::Buffer> = (0..layout.shards())
            .map(|index| {
                gpu.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("state-shard-{index}")),
                    size: shard_bytes,
                    usage: wgpu::BufferUsages::STORAGE
                        | wgpu::BufferUsages::COPY_SRC
                        | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();

        let params = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("gate-params"),
            size: gpu.param_stride * PARAM_SLOTS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let reduce_groups = (shard_len.div_ceil(REDUCE_WG as usize) as u32)
            .min(REDUCE_GROUPS_CAP)
            .min(gpu.max_workgroups())
            .max(1);

        // Blocks never straddle a shard: both lengths are powers of two and the
        // block is the smaller one, so the sampling kernel can be dispatched
        // against a single bound shard.
        let total = 1usize << num_qubits;
        let mut block_len = (total / MAX_BLOCKS)
            .max(REDUCE_WG as usize)
            .next_power_of_two();
        block_len = block_len.min(shard_len);
        let blocks_per_shard = shard_len / block_len;

        let sums_entries =
            (layout.shards() * reduce_groups as usize).max(layout.shards() * blocks_per_shard);
        let sums_bytes = (sums_entries * 4) as u64;
        let sums = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("partial-sums"),
            size: sums_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let sums_read = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("partial-sums-read"),
            size: sums_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let chunk_amplitudes = shard_len.min(CHUNK_AMPLITUDES);
        let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("amplitude-read"),
            size: (chunk_amplitudes * 8) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        if let Some(error) = pollster::block_on(out_of_memory.pop()) {
            return Err(Error::DeviceUnavailable {
                device: "wgpu".to_string(),
                reason: format!(
                    "the adapter could not allocate {} qubits ({} shards of {shard_bytes} B): {error}",
                    num_qubits,
                    layout.shards()
                ),
            });
        }

        let backend = WgpuBackend {
            gpu,
            layout,
            shards,
            params,
            sums,
            sums_read,
            staging,
            reduce_groups,
            block_len,
            blocks_per_shard,
            chunk_amplitudes,
        };
        backend.zero_state();
        Ok(backend)
    }

    /// How the register is split; the tests assert the sharded path really ran.
    pub fn shard_layout(&self) -> ShardLayout {
        self.layout
    }

    /// Block until every kernel submitted so far has finished.
    ///
    /// [`Backend::apply`] only QUEUES work — that is what lets a batch of gates
    /// become one submission — and every read-back path already orders itself
    /// behind the queue. A caller that wants to time a gate rather than the
    /// queuing of one therefore has to close that gap itself, which is what
    /// this is for.
    pub fn sync(&self) {
        self.wait();
    }

    pub fn adapter(&self) -> &'static AdapterReport {
        self.gpu.report()
    }

    // ---- plumbing ----------------------------------------------------------

    fn write_params(&self, slots: &[GpuParams]) {
        let stride = self.gpu.param_stride as usize;
        let mut bytes = vec![0u8; slots.len() * stride];
        for (index, slot) in slots.iter().enumerate() {
            let start = index * stride;
            bytes[start..start + PARAM_BYTES as usize].copy_from_slice(bytemuck::bytes_of(slot));
        }
        self.gpu.queue.write_buffer(&self.params, 0, &bytes);
    }

    fn bind_group(
        &self,
        layout: &wgpu::BindGroupLayout,
        bindings: &[(u32, &wgpu::Buffer)],
    ) -> wgpu::BindGroup {
        let mut entries = Vec::with_capacity(bindings.len() + 1);
        entries.push(wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &self.params,
                offset: 0,
                size: NonZeroU64::new(PARAM_BYTES),
            }),
        });
        for (binding, buffer) in bindings {
            entries.push(wgpu::BindGroupEntry {
                binding: *binding,
                resource: buffer.as_entire_binding(),
            });
        }
        self.gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout,
                entries: &entries,
            })
    }

    fn groups(&self, items: usize, workgroup: u32) -> u32 {
        (items.div_ceil(workgroup as usize) as u64)
            .min(self.gpu.max_workgroups() as u64)
            .max(1) as u32
    }

    fn run(&self, dispatches: &[Dispatch]) {
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            for dispatch in dispatches {
                pass.set_pipeline(dispatch.pipeline);
                pass.set_bind_group(
                    0,
                    &dispatch.bind,
                    &[dispatch.slot * self.gpu.param_stride as u32],
                );
                pass.dispatch_workgroups(dispatch.groups, 1, 1);
            }
        }
        self.gpu.queue.submit([encoder.finish()]);
    }

    fn wait(&self) {
        self.gpu
            .device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("the GPU device stopped answering");
    }

    fn with_mapped<R>(
        &self,
        buffer: &wgpu::Buffer,
        bytes: u64,
        read: impl FnOnce(&[u8]) -> R,
    ) -> R {
        let slice = buffer.slice(0..bytes);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        self.wait();
        receiver
            .recv()
            .expect("the map callback never fired")
            .expect("the GPU refused to map a read-back buffer");
        let view = slice.get_mapped_range();
        let out = read(&view);
        drop(view);
        buffer.unmap();
        out
    }

    fn zero_state(&self) {
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        for shard in &self.shards {
            encoder.clear_buffer(shard, 0, None);
        }
        self.gpu.queue.submit([encoder.finish()]);
        // A queue write lands before the next submission, so the amplitude of
        // |0...0> has to be written AFTER the clear is queued, not before.
        self.gpu
            .queue
            .write_buffer(&self.shards[0], 0, bytemuck::cast_slice(&[1.0f32, 0.0f32]));
        self.gpu.queue.submit([]);
    }

    /// Walk the whole state in host-sized chunks, handing each one to `read` as
    /// interleaved re/im pairs together with the basis index it starts at.
    fn read_chunks(&self, mut read: impl FnMut(usize, &[f32])) {
        let shard_len = self.layout.shard_len();
        for (index, shard) in self.shards.iter().enumerate() {
            let mut offset = 0usize;
            while offset < shard_len {
                let len = self.chunk_amplitudes.min(shard_len - offset);
                let bytes = (len * 8) as u64;
                let mut encoder = self
                    .gpu
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
                encoder.copy_buffer_to_buffer(shard, (offset * 8) as u64, &self.staging, 0, bytes);
                self.gpu.queue.submit([encoder.finish()]);
                let base = self.layout.origin(index) + offset;
                self.with_mapped(&self.staging, bytes, |raw| {
                    read(base, bytemuck::cast_slice(raw));
                });
                offset += len;
            }
        }
    }

    fn read_sums(&self, entries: usize) -> Vec<f32> {
        let bytes = (entries * 4) as u64;
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(&self.sums, 0, &self.sums_read, 0, bytes);
        self.gpu.queue.submit([encoder.finish()]);
        self.with_mapped(&self.sums_read, bytes, |raw| {
            bytemuck::cast_slice::<u8, f32>(raw).to_vec()
        })
    }

    // ---- gates -------------------------------------------------------------

    fn plan_gate(&self, op: &GateOp, slot: u32) -> (GpuParams, Vec<Dispatch>) {
        match op {
            GateOp::One { qubit, matrix } => self.plan_one(*qubit, matrix, slot),
            GateOp::Two { qubits, matrix } => self.plan_two(*qubits, matrix, slot),
        }
    }

    fn plan_one(
        &self,
        qubit: usize,
        matrix: &[Complex64; 4],
        slot: u32,
    ) -> (GpuParams, Vec<Dispatch>) {
        let shard_len = self.layout.shard_len();
        let pipelines = &self.gpu.pipelines;
        if self.layout.is_local(qubit) {
            let count = shard_len / 2;
            let params = GpuParams {
                a: 1u32 << qubit,
                count: count as u32,
                ..GpuParams::default()
            }
            .matrix(matrix);
            let groups = self.groups(count, GATE_WG);
            let dispatches = self
                .shards
                .iter()
                .map(|shard| Dispatch {
                    pipeline: &pipelines.gate1_local,
                    bind: self.bind_group(&self.gpu.layouts.one, &[(1, shard)]),
                    groups,
                    slot,
                })
                .collect();
            (params, dispatches)
        } else {
            let bit = self.layout.shard_bit(qubit);
            let params = GpuParams {
                count: shard_len as u32,
                ..GpuParams::default()
            }
            .matrix(matrix);
            let groups = self.groups(shard_len, GATE_WG);
            let dispatches = (0..self.shards.len())
                .filter(|index| index & bit == 0)
                .map(|index| Dispatch {
                    pipeline: &pipelines.gate1_split,
                    bind: self.bind_group(
                        &self.gpu.layouts.two,
                        &[(1, &self.shards[index]), (2, &self.shards[index | bit])],
                    ),
                    groups,
                    slot,
                })
                .collect();
            (params, dispatches)
        }
    }

    fn plan_two(
        &self,
        qubits: (usize, usize),
        matrix: &[Complex64; 16],
        slot: u32,
    ) -> (GpuParams, Vec<Dispatch>) {
        let (first, second) = qubits;
        // The kernels index the matrix by (high bit, low bit); when the first
        // operand is the low qubit the 01 and 10 basis states swap, exactly as
        // on the CPU.
        let ordered = if first > second {
            *matrix
        } else {
            swap_basis_middle(matrix)
        };
        let high = first.max(second);
        let low = first.min(second);
        let shard_len = self.layout.shard_len();
        let pipelines = &self.gpu.pipelines;

        if self.layout.is_local(high) {
            let count = shard_len / 4;
            let params = GpuParams {
                a: 1u32 << high,
                b: 1u32 << low,
                count: count as u32,
                ..GpuParams::default()
            }
            .matrix(&ordered);
            let groups = self.groups(count, GATE_WG);
            let dispatches = self
                .shards
                .iter()
                .map(|shard| Dispatch {
                    pipeline: &pipelines.gate2_local,
                    bind: self.bind_group(&self.gpu.layouts.one, &[(1, shard)]),
                    groups,
                    slot,
                })
                .collect();
            (params, dispatches)
        } else if self.layout.is_local(low) {
            let high_bit = self.layout.shard_bit(high);
            let count = shard_len / 2;
            let params = GpuParams {
                b: 1u32 << low,
                count: count as u32,
                ..GpuParams::default()
            }
            .matrix(&ordered);
            let groups = self.groups(count, GATE_WG);
            let dispatches = (0..self.shards.len())
                .filter(|index| index & high_bit == 0)
                .map(|index| Dispatch {
                    pipeline: &pipelines.gate2_split_high,
                    bind: self.bind_group(
                        &self.gpu.layouts.two,
                        &[
                            (1, &self.shards[index]),
                            (2, &self.shards[index | high_bit]),
                        ],
                    ),
                    groups,
                    slot,
                })
                .collect();
            (params, dispatches)
        } else {
            let high_bit = self.layout.shard_bit(high);
            let low_bit = self.layout.shard_bit(low);
            let params = GpuParams {
                count: shard_len as u32,
                ..GpuParams::default()
            }
            .matrix(&ordered);
            let groups = self.groups(shard_len, GATE_WG);
            let dispatches = (0..self.shards.len())
                .filter(|index| index & (high_bit | low_bit) == 0)
                .map(|index| Dispatch {
                    pipeline: &pipelines.gate2_split_both,
                    bind: self.bind_group(
                        &self.gpu.layouts.four,
                        &[
                            (1, &self.shards[index]),
                            (2, &self.shards[index | low_bit]),
                            (3, &self.shards[index | high_bit]),
                            (4, &self.shards[index | high_bit | low_bit]),
                        ],
                    ),
                    groups,
                    slot,
                })
                .collect();
            (params, dispatches)
        }
    }

    // ---- reductions --------------------------------------------------------

    /// Sum of `|a|^2` over the whole state, or over the amplitudes whose
    /// `qubit` bit equals `want`.
    fn masked_norm(&self, mask: Option<(usize, bool)>) -> f64 {
        let shard_len = self.layout.shard_len();
        let groups = self.reduce_groups;
        let mut slots = Vec::with_capacity(self.shards.len());
        let mut dispatches = Vec::with_capacity(self.shards.len());
        for (index, shard) in self.shards.iter().enumerate() {
            let (a, b, flag) = match mask {
                None => (0, 1, 0),
                Some((qubit, want)) if self.layout.is_local(qubit) => {
                    (1u32 << qubit, 0, u32::from(want))
                }
                Some((qubit, want)) => {
                    let bit = self.layout.shard_bit(qubit);
                    if ((index & bit) != 0) == want {
                        (0, 1, 0)
                    } else {
                        // Nothing in this shard survives the projection; the
                        // kernel still runs so the partial slot holds a zero
                        // rather than the previous reduction's value.
                        (0, 0, 1)
                    }
                }
            };
            slots.push(GpuParams {
                a,
                b,
                count: shard_len as u32,
                flag,
                base: index as u32 * groups,
                ..GpuParams::default()
            });
            dispatches.push(Dispatch {
                pipeline: &self.gpu.pipelines.reduce,
                bind: self.bind_group(&self.gpu.layouts.reduce, &[(1, shard), (5, &self.sums)]),
                groups,
                slot: index as u32,
            });
        }
        self.write_params(&slots);
        self.run(&dispatches);
        let partials = self.read_sums(self.shards.len() * groups as usize);
        partials.iter().map(|value| *value as f64).sum()
    }
}

impl Backend for WgpuBackend {
    fn name(&self) -> &'static str {
        "wgpu"
    }

    fn adapter_name(&self) -> Option<&str> {
        Some(&self.gpu.report().name)
    }

    fn precision(&self) -> Precision {
        Precision::Single
    }

    fn num_qubits(&self) -> usize {
        self.layout.num_qubits()
    }

    fn reset_to_zero(&mut self) {
        self.zero_state();
    }

    fn set_amplitudes(&mut self, amps: &[Complex64]) {
        debug_assert_eq!(amps.len(), 1usize << self.num_qubits());
        let shard_len = self.layout.shard_len();
        let mut scratch: Vec<f32> = Vec::with_capacity(self.chunk_amplitudes * 2);
        for (index, shard) in self.shards.iter().enumerate() {
            let mut offset = 0usize;
            while offset < shard_len {
                let len = self.chunk_amplitudes.min(shard_len - offset);
                let base = self.layout.origin(index) + offset;
                scratch.clear();
                for value in &amps[base..base + len] {
                    scratch.push(value.re as f32);
                    scratch.push(value.im as f32);
                }
                self.gpu.queue.write_buffer(
                    shard,
                    (offset * 8) as u64,
                    bytemuck::cast_slice(&scratch),
                );
                // Flush so the staged copy of one chunk is released before the
                // next one is queued; a 2 GiB state must not stage 2 GiB.
                self.gpu.queue.submit([]);
                self.wait();
                offset += len;
            }
        }
    }

    fn apply(&mut self, ops: &[GateOp]) {
        for batch in ops.chunks(PARAM_SLOTS) {
            let mut slots = Vec::with_capacity(batch.len());
            let mut dispatches = Vec::new();
            for (index, op) in batch.iter().enumerate() {
                let (params, mut planned) = self.plan_gate(op, index as u32);
                slots.push(params);
                dispatches.append(&mut planned);
            }
            self.write_params(&slots);
            self.run(&dispatches);
        }
    }

    fn apply_global_phase(&mut self, angle: f64) {
        let factor = Complex64::from_polar(1.0, angle);
        let params = GpuParams {
            count: self.layout.shard_len() as u32,
            ..GpuParams::default()
        }
        .matrix(&[factor]);
        let groups = self.groups(self.layout.shard_len(), GATE_WG);
        let dispatches: Vec<Dispatch> = self
            .shards
            .iter()
            .map(|shard| Dispatch {
                pipeline: &self.gpu.pipelines.global_phase,
                bind: self.bind_group(&self.gpu.layouts.one, &[(1, shard)]),
                groups,
                slot: 0,
            })
            .collect();
        self.write_params(&[params]);
        self.run(&dispatches);
    }

    fn probability_of_one(&self, qubit: usize) -> f64 {
        self.masked_norm(Some((qubit, true)))
    }

    fn collapse(&mut self, qubit: usize, outcome: bool) {
        let norm = self.masked_norm(Some((qubit, outcome)));
        debug_assert!(
            norm > 0.0,
            "collapse onto an outcome with zero probability has no normalisation"
        );
        let scale = (1.0 / norm.sqrt()) as f32;
        let shard_len = self.layout.shard_len();
        let groups = self.groups(shard_len, GATE_WG);
        let mut slots = Vec::with_capacity(self.shards.len());
        let mut dispatches = Vec::with_capacity(self.shards.len());
        for (index, shard) in self.shards.iter().enumerate() {
            let (a, flag) = if self.layout.is_local(qubit) {
                (1u32 << qubit, u32::from(outcome))
            } else {
                let bit = self.layout.shard_bit(qubit);
                // `a == 0` makes the kernel decide the whole shard: kept when
                // `flag` is 0, zeroed when it is 1.
                (0u32, u32::from(((index & bit) != 0) != outcome))
            };
            slots.push(GpuParams {
                a,
                count: shard_len as u32,
                flag,
                scale,
                ..GpuParams::default()
            });
            dispatches.push(Dispatch {
                pipeline: &self.gpu.pipelines.collapse,
                bind: self.bind_group(&self.gpu.layouts.one, &[(1, shard)]),
                groups,
                slot: index as u32,
            });
        }
        self.write_params(&slots);
        self.run(&dispatches);
    }

    fn probabilities(&self) -> Vec<f64> {
        let mut out = vec![0.0f64; 1usize << self.num_qubits()];
        self.read_chunks(|base, pairs| {
            for (index, pair) in pairs.as_chunks::<2>().0.iter().enumerate() {
                let (re, im) = (pair[0] as f64, pair[1] as f64);
                out[base + index] = re * re + im * im;
            }
        });
        out
    }

    fn sample(&self, sorted_draws: &[f64]) -> Vec<usize> {
        if sorted_draws.is_empty() {
            return Vec::new();
        }
        let blocks = self.shards.len() * self.blocks_per_shard;
        let prefix = self.block_prefix(blocks);
        let draws = self.locate_draws(sorted_draws, &prefix);
        self.search_blocks(&draws)
    }

    fn read_amplitudes(&self, out: &mut [Complex64]) {
        debug_assert_eq!(out.len(), 1usize << self.num_qubits());
        self.read_chunks(|base, pairs| {
            for (index, pair) in pairs.as_chunks::<2>().0.iter().enumerate() {
                out[base + index] = Complex64::new(pair[0] as f64, pair[1] as f64);
            }
        });
    }
}

impl WgpuBackend {
    /// Exclusive prefix sums of the sampling blocks, accumulated in f64 from the
    /// f32 partials the GPU produced. `prefix[blocks]` is the total mass.
    fn block_prefix(&self, blocks: usize) -> Vec<f64> {
        let mut slots = Vec::with_capacity(self.shards.len());
        let mut dispatches = Vec::with_capacity(self.shards.len());
        let groups = self
            .groups(self.blocks_per_shard, 1)
            .min(self.blocks_per_shard as u32);
        for (index, shard) in self.shards.iter().enumerate() {
            slots.push(GpuParams {
                a: self.block_len as u32,
                count: self.blocks_per_shard as u32,
                base: (index * self.blocks_per_shard) as u32,
                ..GpuParams::default()
            });
            dispatches.push(Dispatch {
                pipeline: &self.gpu.pipelines.block_sums,
                bind: self.bind_group(&self.gpu.layouts.reduce, &[(1, shard), (5, &self.sums)]),
                groups,
                slot: index as u32,
            });
        }
        self.write_params(&slots);
        self.run(&dispatches);

        let partials = self.read_sums(blocks);
        let mut prefix = Vec::with_capacity(blocks + 1);
        let mut running = 0.0f64;
        prefix.push(0.0);
        for value in &partials {
            running += *value as f64;
            prefix.push(running);
        }
        prefix
    }

    /// Turn ascending uniform draws into (block, residual) pairs by walking the
    /// prefix sums once — both sequences are sorted, so this is a merge.
    fn locate_draws(&self, sorted_draws: &[f64], prefix: &[f64]) -> Vec<GpuDraw> {
        let blocks = prefix.len() - 1;
        let last_used = (0..blocks)
            .rev()
            .find(|index| prefix[index + 1] > prefix[*index])
            .unwrap_or(0);
        let mut out = Vec::with_capacity(sorted_draws.len());
        let mut block = 0usize;
        for draw in sorted_draws {
            while block < blocks && *draw >= prefix[block + 1] {
                block += 1;
            }
            if block >= blocks {
                // Rounding can leave the last draws past the accumulated total;
                // they belong to the last block that carries any mass, which is
                // where the missing mass came from.
                out.push(GpuDraw {
                    block: last_used as u32,
                    residual: f32::INFINITY,
                });
            } else {
                out.push(GpuDraw {
                    block: block as u32,
                    residual: (*draw - prefix[block]) as f32,
                });
            }
        }
        out
    }

    /// Run the in-block inverse-CDF search on the GPU, one dispatch per shard
    /// the draws fall into. Draws arrive sorted, so each shard owns one
    /// contiguous range of them.
    fn search_blocks(&self, draws: &[GpuDraw]) -> Vec<usize> {
        let bytes = std::mem::size_of_val(draws) as u64;
        let draw_buffer = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sample-draws"),
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.gpu
            .queue
            .write_buffer(&draw_buffer, 0, bytemuck::cast_slice(draws));
        let picks_bytes = (draws.len() * 4) as u64;
        let picks = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sample-picks"),
            size: picks_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let picks_read = self.gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("sample-picks-read"),
            size: picks_bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut slots = Vec::new();
        let mut dispatches = Vec::new();
        let mut start = 0usize;
        while start < draws.len() {
            let shard = draws[start].block as usize / self.blocks_per_shard;
            let mut end = start;
            while end < draws.len() && draws[end].block as usize / self.blocks_per_shard == shard {
                end += 1;
            }
            let count = end - start;
            slots.push(GpuParams {
                a: self.block_len as u32,
                count: count as u32,
                flag: (shard * self.blocks_per_shard) as u32,
                base: start as u32,
                origin: self.layout.origin(shard) as u32,
                ..GpuParams::default()
            });
            dispatches.push(Dispatch {
                pipeline: &self.gpu.pipelines.sample_search,
                bind: self.bind_group(
                    &self.gpu.layouts.sample,
                    &[(1, &self.shards[shard]), (6, &picks), (7, &draw_buffer)],
                ),
                groups: self.groups(count, GATE_WG),
                slot: (slots.len() - 1) as u32,
            });
            start = end;
            if slots.len() == PARAM_SLOTS {
                self.write_params(&slots);
                self.run(&dispatches);
                slots.clear();
                dispatches.clear();
            }
        }
        if !slots.is_empty() {
            self.write_params(&slots);
            self.run(&dispatches);
        }

        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(&picks, 0, &picks_read, 0, picks_bytes);
        self.gpu.queue.submit([encoder.finish()]);
        self.with_mapped(&picks_read, picks_bytes, |raw| {
            bytemuck::cast_slice::<u8, u32>(raw)
                .iter()
                .map(|index| *index as usize)
                .collect()
        })
    }
}
