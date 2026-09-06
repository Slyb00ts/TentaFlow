// ===== File: sim/wgpu/kernels.wgsl — state-vector kernels, one entry point per gate class =====
//
// The state is `vec2<f32>` amplitudes (plan 18.11: complex64 only on wgpu) held
// in one to four storage buffers. A gate either keeps both members of a pair in
// the SAME shard (`*_local`) or splits them across shards (`*_split_*`), which
// is why the same gate class has more than one entry point: binding exactly the
// buffers a dispatch touches keeps every kernel free of a shard switch and
// needs no binding-array feature.
//
// Every loop is grid-strided over `num_workgroups`, because a 2^28 state needs
// more pair indices than `max_compute_workgroups_per_dimension` allows.

struct Params {
    // Primary stride (elements) for a local gate, or a bit mask for a reduction.
    a: u32,
    // Secondary stride, or the "include everything" switch of `reduce`.
    b: u32,
    // Work items this dispatch owns.
    count: u32,
    // Wanted bit value for a masked reduction / collapse, or the first block
    // index of this shard for `sample_search`.
    flag: u32,
    // Normalisation factor of `collapse`.
    scale: f32,
    // First output slot (reductions) or first draw (sampling) of this dispatch.
    base: u32,
    // First global amplitude index of the bound shard.
    origin: u32,
    pad: u32,
    // Gate matrix, two complex entries per vec4 (uniform arrays stride 16 B).
    m: array<vec4<f32>, 8>,
};

struct Draw {
    block: u32,
    residual: f32,
};

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> s0: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read_write> s1: array<vec2<f32>>;
@group(0) @binding(3) var<storage, read_write> s2: array<vec2<f32>>;
@group(0) @binding(4) var<storage, read_write> s3: array<vec2<f32>>;
@group(0) @binding(5) var<storage, read_write> sums: array<f32>;
@group(0) @binding(6) var<storage, read_write> picks: array<u32>;
@group(0) @binding(7) var<storage, read_write> draws: array<Draw>;

const GATE_WG: u32 = 64u;
const REDUCE_WG: u32 = 256u;

var<workgroup> scratch: array<f32, 256>;

fn cmul(x: vec2<f32>, y: vec2<f32>) -> vec2<f32> {
    return vec2<f32>(x.x * y.x - x.y * y.y, x.x * y.y + x.y * y.x);
}

fn mat(i: u32) -> vec2<f32> {
    let v = p.m[i >> 1u];
    if ((i & 1u) == 0u) {
        return v.xy;
    }
    return v.zw;
}

// Widen `x` by pushing every bit at or above `bit` up one place, leaving a zero
// at `bit`. `bit` is a power of two, so `bit - 1u` is the mask of the bits below
// it and `bit == 1u` degenerates to `x << 1u`, which is what inserting at
// position 0 means.
fn insert_zero(x: u32, bit: u32) -> u32 {
    return ((x & ~(bit - 1u)) << 1u) | (x & (bit - 1u));
}

// ---- one-qubit gates -------------------------------------------------------

@compute @workgroup_size(64)
fn gate1_local(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let span = nwg.x * GATE_WG;
    var t = gid.x;
    while (t < p.count) {
        let lo = insert_zero(t, p.a);
        let hi = lo | p.a;
        let x = s0[lo];
        let y = s0[hi];
        s0[lo] = cmul(mat(0u), x) + cmul(mat(1u), y);
        s0[hi] = cmul(mat(2u), x) + cmul(mat(3u), y);
        t = t + span;
    }
}

@compute @workgroup_size(64)
fn gate1_split(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let span = nwg.x * GATE_WG;
    var t = gid.x;
    while (t < p.count) {
        let x = s0[t];
        let y = s1[t];
        s0[t] = cmul(mat(0u), x) + cmul(mat(1u), y);
        s1[t] = cmul(mat(2u), x) + cmul(mat(3u), y);
        t = t + span;
    }
}

// ---- two-qubit gates -------------------------------------------------------
//
// The matrix row/column index is `2 * high_bit + low_bit`, the order the CPU
// backend normalises to before it reaches a kernel.

fn apply4(v00: vec2<f32>, v01: vec2<f32>, v10: vec2<f32>, v11: vec2<f32>, row: u32) -> vec2<f32> {
    return cmul(mat(row * 4u), v00)
        + cmul(mat(row * 4u + 1u), v01)
        + cmul(mat(row * 4u + 2u), v10)
        + cmul(mat(row * 4u + 3u), v11);
}

@compute @workgroup_size(64)
fn gate2_local(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let span = nwg.x * GATE_WG;
    var t = gid.x;
    while (t < p.count) {
        let i00 = insert_zero(insert_zero(t, p.b), p.a);
        let i01 = i00 | p.b;
        let i10 = i00 | p.a;
        let i11 = i10 | p.b;
        let v00 = s0[i00];
        let v01 = s0[i01];
        let v10 = s0[i10];
        let v11 = s0[i11];
        s0[i00] = apply4(v00, v01, v10, v11, 0u);
        s0[i01] = apply4(v00, v01, v10, v11, 1u);
        s0[i10] = apply4(v00, v01, v10, v11, 2u);
        s0[i11] = apply4(v00, v01, v10, v11, 3u);
        t = t + span;
    }
}

// The high qubit is a shard bit, the low one is not: `s0` holds the high-0 half
// of the quad and `s1` the high-1 half, both at the same offsets.
@compute @workgroup_size(64)
fn gate2_split_high(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let span = nwg.x * GATE_WG;
    var t = gid.x;
    while (t < p.count) {
        let i0 = insert_zero(t, p.b);
        let i1 = i0 | p.b;
        let v00 = s0[i0];
        let v01 = s0[i1];
        let v10 = s1[i0];
        let v11 = s1[i1];
        s0[i0] = apply4(v00, v01, v10, v11, 0u);
        s0[i1] = apply4(v00, v01, v10, v11, 1u);
        s1[i0] = apply4(v00, v01, v10, v11, 2u);
        s1[i1] = apply4(v00, v01, v10, v11, 3u);
        t = t + span;
    }
}

// Both qubits are shard bits: one buffer per corner of the quad.
@compute @workgroup_size(64)
fn gate2_split_both(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let span = nwg.x * GATE_WG;
    var t = gid.x;
    while (t < p.count) {
        let v00 = s0[t];
        let v01 = s1[t];
        let v10 = s2[t];
        let v11 = s3[t];
        s0[t] = apply4(v00, v01, v10, v11, 0u);
        s1[t] = apply4(v00, v01, v10, v11, 1u);
        s2[t] = apply4(v00, v01, v10, v11, 2u);
        s3[t] = apply4(v00, v01, v10, v11, 3u);
        t = t + span;
    }
}

// ---- phase, collapse -------------------------------------------------------

@compute @workgroup_size(64)
fn global_phase(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let span = nwg.x * GATE_WG;
    var t = gid.x;
    while (t < p.count) {
        s0[t] = cmul(mat(0u), s0[t]);
        t = t + span;
    }
}

// Keep the amplitudes whose bit `p.a` equals `p.flag` and rescale them; zero the
// rest. `p.a == 0u` decides the WHOLE shard, which is how a measured qubit that
// is a shard bit is projected: kept with `p.flag == 0u`, dropped otherwise.
@compute @workgroup_size(64)
fn collapse(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let span = nwg.x * GATE_WG;
    var t = gid.x;
    while (t < p.count) {
        if (((t & p.a) != 0u) == (p.flag != 0u)) {
            s0[t] = s0[t] * p.scale;
        } else {
            s0[t] = vec2<f32>(0.0, 0.0);
        }
        t = t + span;
    }
}

// ---- reductions ------------------------------------------------------------

fn workgroup_sum(lid: u32) {
    var stride = REDUCE_WG >> 1u;
    loop {
        if (stride == 0u) {
            break;
        }
        if (lid < stride) {
            scratch[lid] = scratch[lid] + scratch[lid + stride];
        }
        workgroupBarrier();
        stride = stride >> 1u;
    }
}

// Sum |a|^2 over the shard, either wholly (`p.b == 1u`) or only over the
// amplitudes whose bit `p.a` equals `p.flag`. One partial per workgroup; the
// host adds them in f64.
@compute @workgroup_size(256)
fn reduce(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(local_invocation_id) lidv: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let span = nwg.x * REDUCE_WG;
    var acc = 0.0;
    var t = gid.x;
    while (t < p.count) {
        if (p.b == 1u || (((t & p.a) != 0u) == (p.flag != 0u))) {
            let v = s0[t];
            acc = acc + v.x * v.x + v.y * v.y;
        }
        t = t + span;
    }
    scratch[lidv.x] = acc;
    workgroupBarrier();
    workgroup_sum(lidv.x);
    if (lidv.x == 0u) {
        sums[p.base + wid.x] = scratch[0];
    }
}

// Sum |a|^2 over `p.count` contiguous blocks of `p.a` amplitudes, one workgroup
// per block. These are the leaves of the sampling prefix sum.
@compute @workgroup_size(256)
fn block_sums(
    @builtin(local_invocation_id) lidv: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    var blk = wid.x;
    while (blk < p.count) {
        let start = blk * p.a;
        var acc = 0.0;
        var i = lidv.x;
        while (i < p.a) {
            let v = s0[start + i];
            acc = acc + v.x * v.x + v.y * v.y;
            i = i + REDUCE_WG;
        }
        scratch[lidv.x] = acc;
        workgroupBarrier();
        workgroup_sum(lidv.x);
        if (lidv.x == 0u) {
            sums[p.base + blk] = scratch[0];
        }
        workgroupBarrier();
        blk = blk + nwg.x;
    }
}

// Inverse-CDF sampling inside the block the host's prefix sum picked: walk the
// block until the running mass passes the residual. One thread per draw.
@compute @workgroup_size(64)
fn sample_search(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
) {
    let span = nwg.x * GATE_WG;
    var t = gid.x;
    while (t < p.count) {
        let d = draws[p.base + t];
        let start = (d.block - p.flag) * p.a;
        var acc = 0.0;
        var chosen = 0u;
        var i = 0u;
        loop {
            if (i >= p.a) {
                break;
            }
            let v = s0[start + i];
            let pr = v.x * v.x + v.y * v.y;
            if (pr > 0.0) {
                chosen = i;
                acc = acc + pr;
                if (d.residual < acc) {
                    break;
                }
            }
            i = i + 1u;
        }
        picks[p.base + t] = p.origin + start + chosen;
        t = t + span;
    }
}
