// ===== File: msl.rs — Metal Shading Language kernel sources =====
//
// The Metal counterpart of the PTX/HSACO catalogue. Sources are generated from
// a template and compiled by the backend when a model loads, which is what
// replaces a prebuilt artifact set on a platform where compilation is cheap.
//
// Two rules from PLAN_NAPRAWY apply from the first kernel. A variant is a
// parameter, not a copy (§6.3): the scale dtype differs between converters and
// two hand-written kernels would drift apart. And a variant name carries its
// full geometry (§6.4): `_r4_bf16` says four output rows per threadgroup and
// bf16 scales, so a launcher cannot size a grid for a shape the kernel was not
// written for.

use std::fmt;

/// Element type of the scales and zero points as stored in the checkpoint.
/// Not a property of the format: mlx-lm writes bf16, mlx-whisper writes f16,
/// and reading one as the other yields a result with no resemblance to the
/// right answer — measured, not hypothesised.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleDtype {
    F16,
    Bf16,
    /// Normy w GGUF-ie. Skale kwantyzacji nigdy nie są f32 — ten wariant
    /// istnieje dla wag, które nie są skalami, a dzielą z nimi ten parametr.
    F32,
}

impl ScaleDtype {
    /// Metal spelling of the type.
    fn msl(self) -> &'static str {
        match self {
            ScaleDtype::F16 => "half",
            ScaleDtype::Bf16 => "bfloat",
            ScaleDtype::F32 => "float",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            ScaleDtype::F16 => "f16",
            ScaleDtype::Bf16 => "bf16",
            ScaleDtype::F32 => "f32",
        }
    }
}

impl fmt::Display for ScaleDtype {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.suffix())
    }
}

/// Width of one weight code.
///
/// A parameter of the SAME family, not a second one. Both widths compute
/// `q * scale + bias`; six bits only splits the code across two arrays,
/// because a six-bit field straddles word boundaries and the extraction is
/// what limits decode (EKS-A8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bits {
    Four,
    Six,
}

impl Bits {
    pub fn suffix(self) -> &'static str {
        match self {
            Bits::Four => "4bit",
            Bits::Six => "6bit",
        }
    }

    /// Extra buffer the six-bit form reads, declared at `slot`.
    fn high_param(self, slot: u32) -> String {
        match self {
            Bits::Four => String::new(),
            Bits::Six => format!("    device const uint*    high     [[buffer({slot})]],\n"),
        }
    }

    /// Pointer to this row's high bits, or nothing.
    fn high_row(self, rows_expr: &str) -> String {
        match self {
            Bits::Four => String::new(),
            Bits::Six => format!(
                "    device const uint* hi = high + {rows_expr} * (n_cols / 16u);\n"
            ),
        }
    }

    /// The same, when the row is only known inside the loop — the blocked and
    /// matrix forms walk several rows per iteration.
    fn high_word_at(self, row: &str, col: &str) -> String {
        match self {
            Bits::Four => String::new(),
            Bits::Six => format!(
                "const uint hw = high[({row}) * (n_cols / 16u) + ({col}) / 16u]; \
                 const uint hb = (({col}) % 16u) * 2u;"
            ),
        }
    }

    /// Loads the high-bit word covering the eight codes starting at `col0`.
    fn high_word(self, col0: &str) -> String {
        match self {
            Bits::Four => String::new(),
            Bits::Six => format!("const uint hw = hi[({col0}) / 16u]; const uint hb = (({col0}) % 16u) * 2u;"),
        }
    }

    /// The code at slot `j` of the already-loaded word `bits`.
    fn code(self, j: &str) -> String {
        match self {
            Bits::Four => format!("((bits >> ({j} * 4u)) & 0xFu)"),
            Bits::Six => {
                format!("(((bits >> ({j} * 4u)) & 0xFu) | (((hw >> (hb + {j} * 2u)) & 0x3u) << 4u))")
            }
        }
    }
}

/// Element type the kernel writes. A parameter, not a separate family: the
/// previous engine grew a `*_out_f32` twin of every tile that differed by the
/// pointer type and one store, and each pair then drifted apart on its own
/// (PLAN_NAPRAWY §2, D6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutDtype {
    F16,
    F32,
}

impl OutDtype {
    fn msl(self) -> &'static str {
        match self {
            OutDtype::F16 => "half",
            OutDtype::F32 => "float",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            OutDtype::F16 => "f16",
            OutDtype::F32 => "f32",
        }
    }
}

pub use matmul::*;
mod matmul;

/// Threads in one RMSNorm threadgroup. The reduction is per SIMD group and
/// then across the eight of them, so the count is fixed by the kernel body and
/// is not something a caller may pick.
pub const RMSNORM_THREADS: u32 = 256;

/// Root-mean-square normalisation with a learned per-channel weight.
///
/// `y[i] = x[i] * rsqrt(mean(x^2) + eps) * w[i]`
///
/// Accumulates in f32 regardless of the storage type: the mean of squares over
/// four thousand channels is exactly where a narrow accumulator loses the
/// small values, and on Apple f32 accumulation costs 1.3% (EKS-A2).
pub fn rmsnorm_source(weight: ScaleDtype) -> String {
    let ty = weight.msl();
    let name = rmsnorm_name(weight);
    format!(
        r#"
#include <metal_stdlib>
using namespace metal;

kernel void {name}(
    device half*         y      [[buffer(0)]],
    device const half*   x      [[buffer(1)]],
    device const {ty}*   w      [[buffer(2)]],
    constant uint&       n      [[buffer(3)]],
    constant float&      eps    [[buffer(4)]],
    uint tgid [[threadgroup_position_in_grid]],
    uint tid  [[thread_position_in_threadgroup]],
    uint sg   [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]])
{{
    threadgroup float partial[{groups}];

    // Jedna grupa robocza na wiersz. Waga jest wspolna dla wszystkich wierszy,
    // wiec przesuwa sie tylko wejscie i wyjscie.
    x += tgid * n;
    y += tgid * n;

    float sum = 0.0f;
    for (uint i = tid; i < n; i += {threads}u) {{
        const float v = float(x[i]);
        sum = fma(v, v, sum);
    }}
    sum = simd_sum(sum);
    if (lane == 0u) {{ partial[sg] = sum; }}
    threadgroup_barrier(mem_flags::mem_threadgroup);

    float total = 0.0f;
    for (uint i = 0; i < {groups}u; ++i) {{ total += partial[i]; }}
    const float scale = rsqrt(total / float(n) + eps);

    for (uint i = tid; i < n; i += {threads}u) {{
        y[i] = half(float(x[i]) * scale * float(w[i]));
    }}
}}
"#,
        name = name,
        ty = ty,
        threads = RMSNORM_THREADS,
        groups = RMSNORM_THREADS / 32,
    )
}

pub fn rmsnorm_name(weight: ScaleDtype) -> String {
    format!("rmsnorm_t{}_{}", RMSNORM_THREADS, weight.suffix())
}

/// Threads per group for the elementwise gate. Nothing in the body depends on
/// it, so it is a tuning choice rather than a contract.
pub const SILU_MUL_THREADS: u32 = 256;

/// `out[i] = silu(gate[i]) * up[i]`, with `silu(v) = v * sigmoid(v)`.
///
/// The two projections arrive in f32 straight from the GEMV and the result
/// feeds the next one as f16, so the narrowing happens exactly once, here.
pub const SILU_MUL_SOURCE: &str = r#"
#include <metal_stdlib>
using namespace metal;

kernel void silu_mul_f16(
    device half*         out   [[buffer(0)]],
    device const half*   gate  [[buffer(1)]],
    device const half*   up    [[buffer(2)]],
    constant uint&       n     [[buffer(3)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= n) { return; }
    // Wejscia sa w half, a sigmoid liczony w f32: przy prefillu te dwa bufory
    // to najwiekszy pojedynczy ruch pamieci poza samymi wagami, a dokladnosc
    // traci sie dopiero na wykladniku, nie na skladowaniu.
    const float g = float(gate[gid]);
    out[gid] = half(g / (1.0f + exp(-g)) * float(up[gid]));
}
"#;

pub const SILU_MUL_NAME: &str = "silu_mul_f16";

pub fn silu_mul_groups(n: u32) -> u32 {
    n.div_ceil(SILU_MUL_THREADS)
}

/// Threads per head for the rotary embedding: one thread per rotated pair.
pub const ROPE_THREADS_PER_HEAD: u32 = 64;

/// Rotary positional embedding, half-split convention.
///
/// Rotates the pair `(i, i + dims/2)` rather than `(2i, 2i+1)`. The two
/// conventions produce equally plausible tensors of the same shape and differ
/// only in which channels move together, so picking the wrong one yields a
/// model that reads as fluent nonsense — the gate against MLX is the only
/// thing that distinguishes them.
pub const ROPE_HALF_SPLIT_SOURCE: &str = r#"
#include <metal_stdlib>
using namespace metal;

kernel void rope_half_split_f16(
    device half*         v      [[buffer(0)]],
    constant uint&       heads  [[buffer(1)]],
    constant uint&       dims   [[buffer(2)]],
    constant uint&       pos    [[buffer(3)]],
    constant float&      theta  [[buffer(4)]],
    constant uint&       tokens [[buffer(5)]],
    uint gid [[thread_position_in_grid]])
{
    const uint half_dims = dims / 2u;
    const uint per_token = heads * half_dims;
    const uint token = gid / per_token;
    const uint rest  = gid % per_token;
    const uint head  = rest / half_dims;
    const uint i     = rest % half_dims;
    if (token >= tokens) { return; }

    // Czestotliwosc liczona w f32: przy base 1e6 i dims 128 wykladnik schodzi
    // do 1e-6, a w f16 to juz jest zero i caly obrot znika dla polowy kanalow.
    // Kolejne tokeny kafla siedza na kolejnych pozycjach, wiec kat rosnie z nimi.
    const float freq = float(pos + token) * pow(theta, -2.0f * float(i) / float(dims));
    const float c = cos(freq);
    const float s = sin(freq);

    device half* row = v + (token * heads + head) * dims;
    const float x0 = float(row[i]);
    const float x1 = float(row[i + half_dims]);
    row[i]             = half(x0 * c - x1 * s);
    row[i + half_dims] = half(x0 * s + x1 * c);
}
"#;

pub const ROPE_HALF_SPLIT_NAME: &str = "rope_half_split_f16";

pub fn rope_groups(heads: u32, dims: u32, tokens: u32, threads_per_group: u32) -> u32 {
    (tokens * heads * dims / 2).div_ceil(threads_per_group)
}

/// Threads in the argmax threadgroup. One group scans the whole vocabulary.
pub const ARGMAX_THREADS: u32 = 256;

/// Greedy token choice: the index of the largest logit, ties going to the
/// LOWEST index.
///
/// The tie rule is not decoration. Logits collide often enough at low
/// temperature that a different rule changes one token in a few thousand, and
/// a model that diverges once in a while is far harder to debug than one that
/// is wrong always.
pub const ARGMAX_SOURCE: &str = r#"
#include <metal_stdlib>
using namespace metal;

kernel void argmax_f32(
    device uint*         out     [[buffer(0)]],
    device const float*  logits  [[buffer(1)]],
    constant uint&       n       [[buffer(2)]],
    uint tid  [[thread_position_in_threadgroup]],
    uint sg   [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]])
{
    threadgroup float best_val[8];
    threadgroup uint  best_idx[8];

    float val = -INFINITY;
    uint  idx = 0u;
    for (uint i = tid; i < n; i += 256u) {
        const float v = logits[i];
        // Ostra nierownosc: przy rowności zostaje indeks mniejszy, bo ten
        // watek doszedl do niego wczesniej.
        if (v > val) { val = v; idx = i; }
    }

    // Redukcja wewnatrz fali, z ta sama regula remisu.
    for (uint offset = 16u; offset > 0u; offset >>= 1) {
        const float other_val = simd_shuffle_down(val, offset);
        const uint  other_idx = simd_shuffle_down(idx, offset);
        if (other_val > val || (other_val == val && other_idx < idx)) {
            val = other_val;
            idx = other_idx;
        }
    }
    if (lane == 0u) { best_val[sg] = val; best_idx[sg] = idx; }
    threadgroup_barrier(mem_flags::mem_threadgroup);

    if (tid == 0u) {
        float bv = best_val[0];
        uint  bi = best_idx[0];
        for (uint g = 1u; g < 8u; ++g) {
            if (best_val[g] > bv || (best_val[g] == bv && best_idx[g] < bi)) {
                bv = best_val[g];
                bi = best_idx[g];
            }
        }
        out[0] = bi;
    }
}
"#;

pub const ARGMAX_NAME: &str = "argmax_f32";

/// Longest KV cache one attention threadgroup can hold scores for. The score
/// array lives in threadgroup memory, so the bound is real and the kernel
/// REFUSES a longer cache rather than reading past it — the caller chunks.
pub const ATTN_MAX_SEQ: u32 = 2048;
// Uwaga: od czasu softmaxu przyrostowego ta liczba NIE jest ograniczeniem
// kernela — on nie trzyma juz wynikow czastkowych calego wiersza w pamieci
// grupy roboczej. Zostaje jako pojemnosc cache'u, ktora alokuje model.

/// One thread per output channel, which is what makes the second phase a plain
/// parallel loop with no reduction at all.
pub const ATTN_THREADS: u32 = 128;

/// Single-query attention over a whole KV cache, with grouped query heads.
///
/// Three phases inside one threadgroup: scores for every key, softmax over
/// them, then the weighted sum of values. Splitting differently — a thread per
/// key in the last phase — would need an accumulator of `head_dim` floats per
/// thread, which does not fit.
///
/// The query-to-KV mapping is `head / (n_heads / n_kv_heads)`. A wrong mapping
/// changes neither the shape nor the norm of the result; it produces a model
/// reading another head's memory, which is why the gate compares values.
pub fn attn_decode_source(head_dim: u32) -> String {
    let name = attn_decode_name(head_dim);
    format!(
        r#"
#include <metal_stdlib>
using namespace metal;

kernel void {name}(
    device half*         out        [[buffer(0)]],
    device const half*   q          [[buffer(1)]],
    device const half*   k          [[buffer(2)]],
    device const half*   v          [[buffer(3)]],
    constant uint&       n_heads    [[buffer(4)]],
    constant uint&       n_kv_heads [[buffer(5)]],
    constant uint&       seq        [[buffer(6)]],
    constant uint&       seq_cap    [[buffer(7)]],
    constant float&      scale      [[buffer(8)]],
    constant uint&       n_tokens   [[buffer(9)]],
    uint tgid [[threadgroup_position_in_grid]],
    uint tid  [[thread_position_in_threadgroup]],
    uint sg   [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]])
{{
    const uint dim = {dim}u;
    const uint head  = tgid % n_heads;
    const uint token = tgid / n_heads;
    // Dlugosc WAZNA i POJEMNOSC to dwie rozne liczby. Krok skladowania w
    // cache'u wyznacza pojemnosc, a petla po kluczach dlugosc; wziecie jednej
    // za druga adresuje glowice od zlego wiersza i nie zmienia ani ksztaltu,
    // ani rzedu wielkosci wyniku.
    if (token >= n_tokens || seq > seq_cap) {{ return; }}
    const uint kv_head = head / (n_heads / n_kv_heads);

    // Przyczynowosc bez maski: zapytanie kafla siedzi na pozycji
    // `seq - n_tokens + token`, wiec po prostu konczy petle na swojej wlasnej.
    const uint len = seq - n_tokens + token + 1u;

    // Softmax PRZYROSTOWY: maksimum i suma sa poprawiane blok po bloku, a nie
    // liczone na calym wierszu naraz. Poprzednia wersja trzymala wszystkie
    // wyniki czastkowe w pamieci grupy roboczej — {max_seq} floatow, czyli
    // {smem} KB — niezaleznie od tego, ile ich realnie bylo. Przy prefillu to
    // {smem} KB na kazda pare (token, glowica) i zajetosc spadala do kilku grup
    // na rdzen. Teraz starczy jeden blok wynikow, a przy okazji znika limit
    // dlugosci kontekstu wpisany w rozmiar tablicy.
    threadgroup float s[{threads}];
    threadgroup float reduce[{simdgroups}];

    device const half* qh = q + (token * n_heads + head) * dim;
    device const half* kh = k + kv_head * seq_cap * dim;
    device const half* vh = v + kv_head * seq_cap * dim;

    float m = -INFINITY;   // biezace maksimum
    float l = 0.0f;        // biezaca suma wykladnikow
    float o = 0.0f;        // akumulator kanalu tid (tylko tid < dim)

    for (uint j0 = 0; j0 < len; j0 += {threads}u) {{
        const uint j = j0 + tid;
        float sc = -INFINITY;
        if (j < len) {{
            float acc = 0.0f;
            device const half* kj = kh + j * dim;
            for (uint c = 0; c < dim; ++c) {{
                acc = fma(float(qh[c]), float(kj[c]), acc);
            }}
            sc = acc * scale;
        }}

        // Maksimum bloku.
        float bm = simd_max(sc);
        if (lane == 0u) {{ reduce[sg] = bm; }}
        threadgroup_barrier(mem_flags::mem_threadgroup);
        float block_max = reduce[0];
        for (uint g = 1u; g < {simdgroups}u; ++g) {{ block_max = max(block_max, reduce[g]); }}

        const float m_new = max(m, block_max);
        const float rescale = m == -INFINITY ? 0.0f : exp(m - m_new);

        const float e = j < len ? exp(sc - m_new) : 0.0f;
        s[tid] = e;
        float bl = simd_sum(e);
        threadgroup_barrier(mem_flags::mem_threadgroup);
        if (lane == 0u) {{ reduce[sg] = bl; }}
        threadgroup_barrier(mem_flags::mem_threadgroup);
        float block_sum = reduce[0];
        for (uint g = 1u; g < {simdgroups}u; ++g) {{ block_sum += reduce[g]; }}

        l = l * rescale + block_sum;
        if (tid < dim) {{
            float a = o * rescale;
            const uint here = min({threads}u, len - j0);
            for (uint jj = 0; jj < here; ++jj) {{
                a = fma(s[jj], float(vh[(j0 + jj) * dim + tid]), a);
            }}
            o = a;
        }}
        m = m_new;
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }}

    if (tid < dim) {{
        out[(token * n_heads + head) * dim + tid] = half(o / l);
    }}
}}
"#,
        name = name,
        dim = head_dim,
        max_seq = ATTN_MAX_SEQ,
        smem = ATTN_MAX_SEQ * 4 / 1024,
        threads = ATTN_THREADS,
        simdgroups = ATTN_THREADS / 32,
    )
}

/// Queries carried by one threadgroup of the blocked attention kernel.
pub const FLASH_BQ: u32 = 32;
/// Keys processed per inner step.
pub const FLASH_BK: u32 = 32;
/// Four SIMD groups, eight queries each.
pub const FLASH_THREADS: u32 = 128;
/// How far, in log2 units, the running maximum may drift before the output
/// accumulator is rescaled. FlashAttention-4's idea; the threshold matters more
/// here than there, because a Metal fragment cannot be scaled in place.
pub const FLASH_TAU: u32 = 14;

pub fn flash_attn_name(head_dim: u32) -> String {
    format!("flash_attn_prefill_d{head_dim}")
}

/// One threadgroup per (head, block of queries).
pub fn flash_attn_groups(heads: u32, tokens: u32) -> u32 {
    heads * tokens.div_ceil(FLASH_BQ)
}

/// Whether a batch is worth the blocked form. Below a full block of queries it
/// would leave most of the block idle, and the per-token kernel wins.
pub fn flash_fits(tokens: u32, head_dim: u32) -> bool {
    tokens >= FLASH_BQ && head_dim % 8 == 0
}

/// Blocked attention on the SIMD matrix units, for prefill.
///
/// The per-token kernel next door computes one query at a time, each thread
/// walking a whole key row scalar by scalar: measured at 0.41 TFLOPS and 52 ms
/// of a 1226 ms prefill. Here both products are matrix operations — Q·Kᵀ and
/// P·V — with the same tiling and online maximum that FlashAttention describes.
///
/// TWO PASSES over the keys, not one. The single-pass form has to rescale the
/// output accumulator whenever the running maximum moves, and a Metal
/// `simdgroup_matrix` is opaque: scaling it means storing it to threadgroup
/// memory and loading it back. FlashAttention-4 makes that rescale rare with a
/// threshold, which is the right answer where the accumulator lives in tensor
/// memory; here the cheaper trade is to find the maximum first and then build
/// the probabilities against a fixed one. It costs Q·Kᵀ twice — and Q·Kᵀ is
/// half the arithmetic and now runs on the matrix units.
///
/// The exponent goes through `exp2` with `log2(e)` folded into the scale, so
/// the change of base costs nothing. FlashAttention-4 goes further and computes
/// it as a polynomial on the FMA units, because on Blackwell the tensor cores
/// outran the exponential unit by an order of magnitude. On this machine the
/// matrix instruction beats plain FMA by 1.28x (EKS-A2), so that asymmetry does
/// not exist and neither does the reason for the polynomial.
pub fn flash_attn_source(head_dim: u32) -> String {
    let name = flash_attn_name(head_dim);
    format!(
        r#"
#include <metal_stdlib>
#include <metal_simdgroup_matrix>
using namespace metal;

kernel void {name}(
    device half*         out        [[buffer(0)]],
    device const half*   q          [[buffer(1)]],
    device const half*   k          [[buffer(2)]],
    device const half*   v          [[buffer(3)]],
    constant uint&       n_heads    [[buffer(4)]],
    constant uint&       n_kv_heads [[buffer(5)]],
    constant uint&       seq        [[buffer(6)]],
    constant uint&       seq_cap    [[buffer(7)]],
    constant float&      scale      [[buffer(8)]],
    constant uint&       n_tokens   [[buffer(9)]],
    uint2 tgid [[threadgroup_position_in_grid]],
    uint  sg   [[simdgroup_index_in_threadgroup]],
    uint  lane [[thread_index_in_simdgroup]])
{{
    const uint dim = {dim}u;
    const uint BQ  = {bq}u;
    const uint BK  = {bk}u;
    const uint blocks_q = (n_tokens + BQ - 1u) / BQ;
    const uint head = tgid.x / blocks_q;
    const uint qb   = tgid.x % blocks_q;
    if (head >= n_heads || seq > seq_cap) {{ return; }}
    const uint kv_head = head / (n_heads / n_kv_heads);
    const uint tid = sg * 32u + lane;

    const uint q0 = qb * BQ;
    // Pozycja bezwzgledna zapytania: kafel siedzi na koncu kontekstu.
    const uint base_pos = seq - n_tokens;

    device const half* qh = q + head * dim;          // krok wiersza: n_heads*dim
    device const half* kh = k + kv_head * seq_cap * dim;
    device const half* vh = v + kv_head * seq_cap * dim;
    const uint q_stride = n_heads * dim;

    // Ten bufor sluzy dwóm rzeczom: wynikom Q·Kᵀ w petli i zapisowi wyjscia po
    // niej. To drugie potrzebuje osmiu zapytan po `dim` kanalow, wiec rozmiar
    // jest MAKSIMUM z obu — przy malym bloku kluczy sam iloczyn by wystarczyl,
    // a wyjscie pisaloby poza tablice.
    threadgroup float sbuf[{sbuf}u];
    threadgroup half  pbuf[{bq}u * {bk}u];
    threadgroup float mbuf[{bq}u];
    threadgroup float lbuf[{bq}u];
    // Redukcja czastkowa: kazdy wiersz zapytania obsluguja CZTERY watki, po
    // osmiu kluczach kazdy. Przy jednym watku na wiersz pracowala jedna grupa
    // SIMD z czterech, a lancuch exp2 byl czterokrotnie dluzszy.
    threadgroup float rmax[{bq}u * 4u];
    threadgroup float rsum[{bq}u * 4u];
    threadgroup float fbuf[{bq}u];
    threadgroup float obuf[8u * {dim}u];
    threadgroup uint  flag[1];

    const uint sg_q = sg * 8u;                        // osiem zapytan tej grupy
    const uint my_q = q0 + tid;                       // wiersz obslugiwany w redukcjach
    const uint my_pos = base_pos + my_q;
    const uint my_len = my_q < n_tokens ? my_pos + 1u : 0u;

    // Skala z wpisanym log2(e): dalej wystarczy exp2.
    const float scale2 = scale * 1.4426950408889634f;

    if (tid < BQ) {{ mbuf[tid] = -INFINITY; lbuf[tid] = 0.0f; }}
    if (tid == 0u) {{ flag[0] = 0u; }}
    threadgroup_barrier(mem_flags::mem_threadgroup);


    // Najdluzszy wiersz w bloku wyznacza, ile blokow kluczy trzeba przejsc.
    const uint last_q = min(q0 + BQ, n_tokens) - 1u;
    const uint len_max = base_pos + last_q + 1u;

    simdgroup_matrix<float, 8, 8> oacc[{d_frags}];
    for (uint d = 0; d < {d_frags}u; ++d) {{ oacc[d] = simdgroup_matrix<float, 8, 8>(0); }}

    for (uint j0 = 0; j0 < len_max; j0 += BK) {{
        simdgroup_matrix<float, 8, 8> sacc[{bk_frags}];
        for (uint n = 0; n < {bk_frags}u; ++n) {{ sacc[n] = simdgroup_matrix<float, 8, 8>(0); }}
        for (uint c = 0; c < dim; c += 8u) {{
            simdgroup_matrix<half, 8, 8> qf, kf;
            simdgroup_load(qf, qh + (q0 + sg_q) * q_stride + c, q_stride);
            for (uint n = 0; n < {bk_frags}u; ++n) {{
                simdgroup_load(kf, kh + (j0 + n * 8u) * dim + c, dim, ulong2(0, 0), true);
                simdgroup_multiply_accumulate(sacc[n], qf, kf, sacc[n]);
            }}
        }}
        threadgroup_barrier(mem_flags::mem_threadgroup);
        for (uint n = 0; n < {bk_frags}u; ++n) {{
            simdgroup_store(sacc[n], sbuf + sg_q * BK + n * 8u, BK);
        }}
        threadgroup_barrier(mem_flags::mem_threadgroup);

        // Maksimum bloku, liczone przez CZTERY watki na wiersz zapytania. Przy
        // jednym pracowala jedna grupa SIMD z czterech, a lancuch exp2 byl
        // czterokrotnie dluzszy.
        const uint qrow = tid % BQ;
        const uint part = tid / BQ;
        const uint q_len = q0 + qrow < n_tokens ? base_pos + q0 + qrow + 1u : 0u;
        const uint here = min(BK, q_len > j0 ? q_len - j0 : 0u);
        const uint n_lo = part * (BK / 4u);
        const uint n_hi = min(n_lo + BK / 4u, here);
        {{
            float bm = -INFINITY;
            for (uint n = n_lo; n < n_hi; ++n) {{
                bm = max(bm, sbuf[qrow * BK + n] * scale2);
            }}
            rmax[qrow * 4u + part] = bm;
        }}
        threadgroup_barrier(mem_flags::mem_threadgroup);

        // Przeskalowanie WARUNKOWE — pomysl z FlashAttention-4. Akumulator
        // zostaje w starej bazie, dopoki nowe maksimum nie przerosnie jej o
        // wiecej niz {tau} w skali log2; do tego progu wykladniki mieszcza sie
        // bez ryzyka. Tutaj to oszczedza wiecej niz na Blackwellu: fragment
        // Metala jest NIEPRZEZROCZYSTY, wiec kazde przeskalowanie oznacza
        // przepuszczenie calego akumulatora przez pamiec grupy roboczej.
        if (tid < BQ) {{
            float bm = rmax[tid * 4u];
            for (uint g = 1u; g < 4u; ++g) {{ bm = max(bm, rmax[tid * 4u + g]); }}
            const float m_old = mbuf[tid];
            const bool need = m_old == -INFINITY || bm > m_old + {tau}.0f;
            const float m_new = need ? max(m_old, bm) : m_old;
            fbuf[tid] = m_old == -INFINITY ? 0.0f : exp2(m_old - m_new);
            mbuf[tid] = m_new;
            if (need) {{ flag[0] = 1u; }}
        }}
        if (tid == 0u && j0 == 0u) {{ flag[0] = 1u; }}
        threadgroup_barrier(mem_flags::mem_threadgroup);

        if (flag[0] != 0u) {{
            for (uint g = 0; g < 4u; ++g) {{
                threadgroup_barrier(mem_flags::mem_threadgroup);
                if (sg == g) {{
                    for (uint d = 0; d < {d_frags}u; ++d) {{
                        simdgroup_store(oacc[d], obuf + d * 8u, dim);
                    }}
                }}
                threadgroup_barrier(mem_flags::mem_threadgroup);
                for (uint i = tid; i < 8u * dim; i += {threads}u) {{
                    obuf[i] *= fbuf[g * 8u + i / dim];
                }}
                threadgroup_barrier(mem_flags::mem_threadgroup);
                if (sg == g) {{
                    for (uint d = 0; d < {d_frags}u; ++d) {{
                        simdgroup_load(oacc[d], obuf + d * 8u, dim);
                    }}
                }}
            }}
            if (tid < BQ) {{ lbuf[tid] *= fbuf[tid]; }}
            threadgroup_barrier(mem_flags::mem_threadgroup);
            if (tid == 0u) {{ flag[0] = 0u; }}
        }}
        threadgroup_barrier(mem_flags::mem_threadgroup);

        // Prawdopodobienstwa w BIEZACEJ bazie. Maska przyczynowa przez zero, a
        // nie przez minus nieskonczonosc: to samo, a exp2 nie widzi NaN.
        {{
            const float m = mbuf[qrow];
            float bl = 0.0f;
            for (uint n = part * (BK / 4u); n < (part + 1u) * (BK / 4u); ++n) {{
                const float e =
                    n < here ? exp2(sbuf[qrow * BK + n] * scale2 - m) : 0.0f;
                pbuf[qrow * BK + n] = half(e);
                bl += e;
            }}
            rsum[qrow * 4u + part] = bl;
        }}
        threadgroup_barrier(mem_flags::mem_threadgroup);
        if (tid < BQ) {{
            float bl = 0.0f;
            for (uint g = 0; g < 4u; ++g) {{ bl += rsum[tid * 4u + g]; }}
            lbuf[tid] += bl;
        }}
        threadgroup_barrier(mem_flags::mem_threadgroup);

        for (uint n = 0; n < {bk_frags}u; ++n) {{
            simdgroup_matrix<half, 8, 8> pf, vf;
            simdgroup_load(pf, pbuf + sg_q * BK + n * 8u, BK);
            for (uint d = 0; d < {d_frags}u; ++d) {{
                simdgroup_load(vf, vh + (j0 + n * 8u) * dim + d * 8u, dim);
                simdgroup_multiply_accumulate(oacc[d], pf, vf, oacc[d]);
            }}
        }}
        threadgroup_barrier(mem_flags::mem_threadgroup);
    }}

    // Wyjscie grupa po grupie przez bufor wspolny: `simdgroup_store` wymaga celu
    // typu akumulatora, a wyjscie jest w half. Osiem zapytan po `dim` kanalow to
    // dokladnie tyle floatow, ile ma `sbuf`, wiec nie trzeba nowej pamieci.
    for (uint g = 0; g < 4u; ++g) {{
        threadgroup_barrier(mem_flags::mem_threadgroup);
        if (sg == g) {{
            for (uint d = 0; d < {d_frags}u; ++d) {{
                simdgroup_store(oacc[d], sbuf + d * 8u, dim);
            }}
        }}
        threadgroup_barrier(mem_flags::mem_threadgroup);
        for (uint i = tid; i < 8u * dim; i += {threads}u) {{
            const uint qi = g * 8u + i / dim;
            if (q0 + qi < n_tokens) {{
                out[(q0 + qi) * q_stride + head * dim + i % dim] =
                    half(sbuf[i] / lbuf[qi]);
            }}
        }}
    }}
}}
"#,
        name = name,
        dim = head_dim,
        bq = FLASH_BQ,
        bk = FLASH_BK,
        bk_frags = FLASH_BK / 8,
        sbuf = (FLASH_BQ * FLASH_BK).max(8 * head_dim),
        tau = FLASH_TAU,
        d_frags = head_dim / 8,
        threads = FLASH_THREADS,
    )
}

/// One threadgroup per (token, head). Derived so a caller cannot size the grid
/// for a batch the kernel was not given.
pub fn attn_groups(heads: u32, tokens: u32) -> u32 {
    heads * tokens
}

pub fn attn_decode_name(head_dim: u32) -> String {
    format!("attn_decode_online_d{head_dim}")
}

/// Threads per group for the embedding lookup and the residual add. Both are
/// plain elementwise passes, so the count is a tuning choice.
pub const ELEMENTWISE_THREADS: u32 = 256;

/// Dequantizes ONE row of a quantized embedding table.
///
/// In MLX the embedding is quantized exactly like a projection, so a lookup is
/// a dequantize of a single row. It is also the only read in a decode step
/// indexed by a token rather than sequential, which makes a wrong row offset
/// produce a correctly shaped vector holding someone else's word.
pub fn embed_gather_source(scales: ScaleDtype) -> String {
    let ty = scales.msl();
    let name = embed_gather_name(scales);
    format!(
        r#"
#include <metal_stdlib>
using namespace metal;

kernel void {name}(
    device half*          out      [[buffer(0)]],
    device const uint*    packed   [[buffer(1)]],
    device const {ty}*    scales   [[buffer(2)]],
    device const {ty}*    biases   [[buffer(3)]],
    device const uint*    tokens   [[buffer(4)]],
    constant uint&        hidden   [[buffer(5)]],
    constant uint&        group    [[buffer(6)]],
    constant uint&        n_tokens [[buffer(7)]],
    uint gid [[thread_position_in_grid]])
{{
    const uint row = gid / hidden;
    const uint col = gid % hidden;
    if (row >= n_tokens) {{ return; }}
    // Identyfikatory ida buforem, a nie skalarem, bo prefill podaje ich naraz
    // caly kafel. Przy jednym tokenie to ten sam kernel i ten sam wynik.
    const uint token = tokens[row];
    const uint words_per_row  = hidden / 8u;
    const uint groups_per_row = hidden / group;

    const uint word = col / 8u;
    const uint slot = col % 8u;
    const uint bits = packed[token * words_per_row + word];
    const float q = float((bits >> (slot * 4u)) & 0xFu);

    const uint g = col / group;
    const float sc = float(scales[token * groups_per_row + g]);
    const float bi = float(biases[token * groups_per_row + g]);
    out[gid] = half(fma(q, sc, bi));
}}
"#,
        name = name,
        ty = ty,
    )
}

pub fn embed_gather_name(scales: ScaleDtype) -> String {
    format!("embed_gather_4bit_{}", scales.suffix())
}

/// `out[i] = a[i] + b[i]` over f16, which is the residual connection.
///
/// The sum is formed in f32 and narrowed once. Adding in f16 loses the small
/// residual against a large activation, and the residual is precisely the part
/// that carries information across forty layers.
pub const RESIDUAL_ADD_SOURCE: &str = r#"
#include <metal_stdlib>
using namespace metal;

kernel void residual_add_f16(
    device half*         out  [[buffer(0)]],
    device const half*   a    [[buffer(1)]],
    device const float*  b    [[buffer(2)]],
    constant uint&       n    [[buffer(3)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= n) { return; }
    out[gid] = half(float(a[gid]) + b[gid]);
}
"#;

pub const RESIDUAL_ADD_NAME: &str = "residual_add_f16";

pub fn elementwise_groups(n: u32) -> u32 {
    n.div_ceil(ELEMENTWISE_THREADS)
}

/// Writes one position's keys or values into the KV cache.
///
/// The source is `[kv_head][dim]` contiguous and the destination is
/// `[kv_head][seq][dim]`, so the write is strided. Doing it with one copy per
/// head would mean sixteen device copies per layer per token — the previous
/// engine measured 430 such copies per token to move 8 KiB (PLAN_NAPRAWY §3 pkt 3).
pub const KV_APPEND_SOURCE: &str = r#"
#include <metal_stdlib>
using namespace metal;

kernel void kv_append_f16(
    device half*         cache    [[buffer(0)]],
    device const half*   src      [[buffer(1)]],
    constant uint&       kv_heads [[buffer(2)]],
    constant uint&       dim      [[buffer(3)]],
    constant uint&       seq_cap  [[buffer(4)]],
    constant uint&       pos      [[buffer(5)]],
    constant uint&       tokens   [[buffer(6)]],
    uint gid [[thread_position_in_grid]])
{
    const uint total = kv_heads * dim;
    const uint token = gid / total;
    const uint rest  = gid % total;
    if (token >= tokens || pos + token >= seq_cap) { return; }
    const uint head = rest / dim;
    const uint c    = rest % dim;
    cache[(head * seq_cap + pos + token) * dim + c] = src[gid];
}
"#;

pub const KV_APPEND_NAME: &str = "kv_append_f16";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_covers_every_row_including_a_ragged_tail() {
        assert_eq!(qmv_affine_4bit_groups(128), 32);
        assert_eq!(qmv_affine_4bit_groups(4), 1);
        // Sześć wierszy to dwie grupy, z których druga liczy tylko dwa —
        // kernel maskuje nadmiar, a nie odmawia.
        assert_eq!(qmv_affine_4bit_groups(6), 2);
        assert_eq!(qmv_affine_4bit_groups(1), 1);
    }

    #[test]
    fn the_name_carries_geometry_and_scale_type() {
        assert_eq!(
            qmv_affine_name(Bits::Four, ScaleDtype::Bf16, OutDtype::F32),
            "qmv_affine_4bit_r4_bf16_outf32"
        );
        assert_eq!(
            qmv_affine_name(Bits::Four, ScaleDtype::F16, OutDtype::F16),
            "qmv_affine_4bit_r4_f16_outf16"
        );
        assert_eq!(QMV_THREADS, QMV_ROWS_PER_GROUP * 32);
    }

    #[test]
    fn rmsnorm_name_and_body_agree_on_the_thread_count() {
        let src = rmsnorm_source(ScaleDtype::Bf16);
        assert_eq!(rmsnorm_name(ScaleDtype::Bf16), "rmsnorm_t256_bf16");
        assert!(src.contains("rmsnorm_t256_bf16"));
        // Krok pętli MUSI być tą samą liczbą co w nazwie: rozjazd zostawiłby
        // część kanałów niepoliczonych, bez żadnego błędu.
        assert!(src.contains(&format!("i += {RMSNORM_THREADS}u")));
        assert!(src.contains(&format!("threadgroup float partial[{}]", RMSNORM_THREADS / 32)));
    }

    #[test]
    fn silu_mul_grid_covers_a_ragged_tail() {
        assert_eq!(silu_mul_groups(11264), 44);
        assert_eq!(silu_mul_groups(1), 1);
        assert_eq!(silu_mul_groups(257), 2);
        assert!(SILU_MUL_SOURCE.contains(SILU_MUL_NAME));
    }

    #[test]
    fn rope_grid_covers_every_rotated_pair() {
        // 32 głowice po 128 wymiarów to 2048 par, czyli 8 grup po 256 wątków.
        assert_eq!(rope_groups(32, 128, 1, 256), 8);
        assert_eq!(rope_groups(1, 128, 1, 64), 1);
        // Kafel tokenów mnoży pracę, a nie dzieli ją między te same wątki.
        assert_eq!(rope_groups(32, 128, 8, 256), 64);
        assert!(ROPE_HALF_SPLIT_SOURCE.contains(ROPE_HALF_SPLIT_NAME));
        // Para to (i, i + dims/2), nie (2i, 2i+1) — gdyby ktoś przepisał kernel
        // na wariant sąsiadujący, ten warunek to złapie zanim złapie to model.
        assert!(ROPE_HALF_SPLIT_SOURCE.contains("row[i + half_dims]"));
    }

    #[test]
    fn argmax_declares_its_tie_rule() {
        assert!(ARGMAX_SOURCE.contains(ARGMAX_NAME));
        assert!(ARGMAX_SOURCE.contains("other_idx < idx"));
        assert_eq!(ARGMAX_THREADS, 256);
    }

    #[test]
    fn attention_name_carries_the_bounds_the_body_enforces() {
        let src = attn_decode_source(128);
        // Nazwa nie niesie juz limitu dlugosci, bo kernel go nie ma: softmax
        // przyrostowy trzyma stan o rozmiarze grupy roboczej, a nie wiersza.
        assert_eq!(attn_decode_name(128), "attn_decode_online_d128");
        assert!(src.contains("attn_decode_online_d128"));
        // Wynikow czastkowych jest tyle, ile watkow, a nie tyle, ile kluczy:
        // to jest cala roznica miedzy softmaxem przyrostowym a poprzednim.
        assert!(src.contains(&format!("threadgroup float s[{ATTN_THREADS}]")));
        assert!(!src.contains("scores["), "wrocila tablica na caly wiersz");
        // Dlugosc nadal nie moze przekroczyc pojemnosci cache'u.
        assert!(src.contains("seq > seq_cap"));
        // Krok składowania liczy się z POJEMNOŚCI, nie z bieżącej długości.
        assert!(src.contains("kv_head * seq_cap * dim"));
        assert!(!src.contains("kv_head * seq * dim"));
        assert!(src.contains("head / (n_heads / n_kv_heads)"));
    }

    #[test]
    fn output_type_is_a_parameter_not_a_second_family() {
        let a = qmv_affine_source(Bits::Four, ScaleDtype::Bf16, OutDtype::F32);
        let b = qmv_affine_source(Bits::Four, ScaleDtype::Bf16, OutDtype::F16);
        // Poza typem zapisu i nazwą oba źródła są identyczne — to jest różnica
        // między parametrem a drugą rodziną kerneli, która potem żyje własnym
        // życiem i rozjeżdża się przy pierwszej poprawce.
        //
        // Podmiana obejmuje DOKŁADNIE dwa miejsca: deklarację wyjścia i zapis.
        // Podmiana samego napisu „float" trafiłaby też w `bfloat` i w
        // akumulatory, a wtedy test porównywałby dwa równie zniekształcone
        // teksty i przechodził niezależnie od tego, co robi generator.
        let strip = |src: &str, out: OutDtype| {
            let ty = out.msl();
            src.replace(
                &format!("device {ty}*      y"),
                "device OUT_T*      y",
            )
            .replace(
                &format!("y[row] = {ty}(total)"),
                "y[row] = OUT_T(total)",
            )
            .replace(&qmv_affine_name(Bits::Four, ScaleDtype::Bf16, out), "ENTRY")
        };
        let stripped_a = strip(&a, OutDtype::F32);
        assert!(stripped_a.contains("device OUT_T*      y"), "podmiana nie trafiła");
        assert!(stripped_a.contains("y[row] = OUT_T(total)"), "podmiana nie trafiła");
        assert_eq!(stripped_a, strip(&b, OutDtype::F16));
    }

    #[test]
    fn kv_append_scatters_by_head_and_position() {
        assert!(KV_APPEND_SOURCE.contains(KV_APPEND_NAME));
        // Adres w cache'u musi zawierać i głowicę, i pozycję; pominięcie
        // którejkolwiek nadpisuje cudzy wpis bez żadnego objawu.
        assert!(KV_APPEND_SOURCE.contains("(head * seq_cap + pos + token) * dim + c"));
        assert!(KV_APPEND_SOURCE.contains("pos + token >= seq_cap"));
    }

    #[test]
    fn embedding_lookup_indexes_by_token_in_both_arrays() {
        let src = embed_gather_source(ScaleDtype::Bf16);
        assert_eq!(embed_gather_name(ScaleDtype::Bf16), "embed_gather_4bit_bf16");
        // Wiersz wybiera token — i w upakowanych bitach, i w skalach. Pominięcie
        // przesunięcia w którejkolwiek z tych tablic daje wektor o poprawnym
        // kształcie zbudowany z cudzych liczb.
        assert!(src.contains("packed[token * words_per_row + word]"));
        assert!(src.contains("scales[token * groups_per_row + g]"));
        assert!(src.contains("biases[token * groups_per_row + g]"));
    }

    #[test]
    fn residual_sums_in_f32_before_narrowing() {
        assert!(RESIDUAL_ADD_SOURCE.contains(RESIDUAL_ADD_NAME));
        assert!(RESIDUAL_ADD_SOURCE.contains("half(float(a[gid]) + b[gid])"));
        assert_eq!(elementwise_groups(4096), 16);
        assert_eq!(elementwise_groups(1), 1);
    }

    #[test]
    fn both_variants_come_from_one_template() {
        let f16 = qmv_affine_source(Bits::Four, ScaleDtype::F16, OutDtype::F32);
        let bf16 = qmv_affine_source(Bits::Four, ScaleDtype::Bf16, OutDtype::F32);
        assert!(f16.contains("device const half*    scales"));
        assert!(bf16.contains("device const bfloat*    scales"));
        // Poza nazwą i typem skal źródła muszą być identyczne: dwie ręcznie
        // pisane kopie rozjeżdżają się przy pierwszej poprawce.
        let norm = |s: &str, ty: &str, name: &str| {
            s.replace(ty, "SCALE_T").replace(name, "ENTRY")
        };
        assert_eq!(
            norm(&f16, "half*    scales", &qmv_affine_name(Bits::Four, ScaleDtype::F16, OutDtype::F32))
                .replace("half*    biases", "SCALE_T biases")
                .replace("const half* s", "const SCALE_T s")
                .replace("const half* b", "const SCALE_T b"),
            norm(
                &bf16,
                "bfloat*    scales",
                &qmv_affine_name(Bits::Four, ScaleDtype::Bf16, OutDtype::F32)
            )
            .replace("bfloat*    biases", "SCALE_T biases")
            .replace("const bfloat* s", "const SCALE_T s")
            .replace("const bfloat* b", "const SCALE_T b")
        );
    }
}
