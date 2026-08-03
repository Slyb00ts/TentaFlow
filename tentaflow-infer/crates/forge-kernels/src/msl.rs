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
}

impl ScaleDtype {
    /// Metal spelling of the type.
    fn msl(self) -> &'static str {
        match self {
            ScaleDtype::F16 => "half",
            ScaleDtype::Bf16 => "bfloat",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            ScaleDtype::F16 => "f16",
            ScaleDtype::Bf16 => "bf16",
        }
    }
}

impl fmt::Display for ScaleDtype {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.suffix())
    }
}

/// Output rows handled by one threadgroup: one SIMD group per row.
pub const QMV_ROWS_PER_GROUP: u32 = 4;
/// Four SIMD groups of 32 lanes.
pub const QMV_THREADS: u32 = QMV_ROWS_PER_GROUP * 32;

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

/// Entry-point name for a given scale type and output type.
pub fn qmv_affine_4bit_name(scales: ScaleDtype, out: OutDtype) -> String {
    format!("qmv_affine_4bit_r4_{}_out{}", scales.suffix(), out.suffix())
}

/// Grid for a given output width. Derived, never written out by hand.
pub fn qmv_affine_4bit_groups(n_rows: u32) -> u32 {
    n_rows.div_ceil(QMV_ROWS_PER_GROUP)
}

/// Fused dequantize + matrix-vector product for MLX affine weights.
///
/// `y[n] = sum_k x[k] * (q[n,k] * scale[n,k/G] + bias[n,k/G])`
///
/// The weight is never materialised: each thread unpacks the nibbles it needs
/// straight out of the packed word. Reading the whole matrix once is the entire
/// cost of a decode step, so anything that writes a dequantized copy first
/// doubles the traffic that dominates it.
pub fn qmv_affine_4bit_source(scales: ScaleDtype, out: OutDtype) -> String {
    let ty = scales.msl();
    let out_ty = out.msl();
    let name = qmv_affine_4bit_name(scales, out);
    format!(
        r#"
#include <metal_stdlib>
using namespace metal;

kernel void {name}(
    device {out_ty}*      y        [[buffer(0)]],
    device const uint*    packed   [[buffer(1)]],
    device const {ty}*    scales   [[buffer(2)]],
    device const {ty}*    biases   [[buffer(3)]],
    device const half*    x        [[buffer(4)]],
    constant uint&        n_rows   [[buffer(5)]],
    constant uint&        n_cols   [[buffer(6)]],
    constant uint&        group    [[buffer(7)]],
    uint tgid  [[threadgroup_position_in_grid]],
    uint sg    [[simdgroup_index_in_threadgroup]],
    uint lane  [[thread_index_in_simdgroup]])
{{
    const uint row = tgid * {rows}u + sg;
    if (row >= n_rows) {{ return; }}

    const uint words_per_row  = n_cols / 8u;      // osiem wag 4-bitowych na slowo
    const uint groups_per_row = n_cols / group;
    device const uint* w = packed + row * words_per_row;
    device const {ty}* s = scales + row * groups_per_row;
    device const {ty}* b = biases + row * groups_per_row;

    float acc = 0.0f;
    // Kolejne linie czytaja kolejne slowa, wiec odczyt jest sklejony.
    for (uint word = lane; word < words_per_row; word += 32u) {{
        const uint bits = w[word];
        const uint col0 = word * 8u;
        const uint g    = col0 / group;
        const float sc  = float(s[g]);
        const float bi  = float(b[g]);
        for (uint j = 0; j < 8u; ++j) {{
            const float q = float((bits >> (j * 4u)) & 0xFu);
            acc += float(x[col0 + j]) * fma(q, sc, bi);
        }}
    }}

    const float total = simd_sum(acc);
    if (lane == 0u) {{ y[row] = {out_ty}(total); }}
}}
"#,
        name = name,
        ty = ty,
        out_ty = out_ty,
        rows = QMV_ROWS_PER_GROUP,
    )
}

/// Rows of activation one threadgroup carries through a single pass over the
/// weights. This constant IS the prefill lever: a decode step reads the whole
/// matrix to serve one token, so eight tokens sharing that read cut the traffic
/// that dominates prefill by eight. Larger tiles keep paying off arithmetically
/// but cost a register per token per lane, and eight fits without spilling.
pub const QMM_TILE: u32 = 8;

/// Output rows one SIMD GROUP carries at once.
///
/// The third knob, and it works on the instruction mix rather than on traffic.
/// With one row per group every activation value feeds exactly one multiply, so
/// the loop issues roughly as many loads as FMAs and cannot get near the ALU.
/// Two rows share each loaded activation between two multiplies.
pub const QMM_ROWS_PER_SIMD: u32 = 2;

/// Output rows one threadgroup carries.
///
/// This is the second knob, and it works on a different term than the tile.
/// The tile decides how many tokens share one read of the weights; this decides
/// how many output rows share one read of the ACTIVATIONS. At large batches the
/// activation term is the one that dominates — with four rows a 512-token chunk
/// re-reads its activations a thousand times over, which is exactly why the
/// first version regressed there.
pub const QMM_ROWS_PER_GROUP: u32 = 32;
/// Threads per batched threadgroup: 32 lanes per SIMD group.
pub const QMM_THREADS: u32 = (QMM_ROWS_PER_GROUP / QMM_ROWS_PER_SIMD) * 32;

/// Entry-point name for the batched form.
pub fn qmm_affine_4bit_name(scales: ScaleDtype, out: OutDtype) -> String {
    format!(
        "qmm_affine_4bit_r{}s{}t{}_{}_out{}",
        QMM_ROWS_PER_GROUP,
        QMM_ROWS_PER_SIMD,
        QMM_TILE,
        scales.suffix(),
        out.suffix()
    )
}

/// Grid for an output width and a token count. Two dimensions, both derived.
pub fn qmm_affine_4bit_groups(n_rows: u32, n_tokens: u32) -> (u32, u32) {
    (
        n_rows.div_ceil(QMM_ROWS_PER_GROUP),
        n_tokens.div_ceil(QMM_TILE),
    )
}

/// Fused dequantize + matrix-matrix product for MLX affine weights.
///
/// `y[t,n] = sum_k x[t,k] * (q[n,k] * scale[n,k/G] + bias[n,k/G])`
///
/// Same arithmetic as the vector form, same accumulation order per output, so
/// prefill and decode agree bit for bit on a shared position — which is what
/// makes it legitimate to test one against the other.
///
/// The unpacked weight stays in a register and is applied to every token in the
/// tile before the next word is read. That ordering is the whole point: it is
/// what turns one pass over a multi-gigabyte matrix into eight tokens of work
/// instead of one.
pub fn qmm_affine_4bit_source(scales: ScaleDtype, out: OutDtype) -> String {
    let ty = scales.msl();
    let out_ty = out.msl();
    let name = qmm_affine_4bit_name(scales, out);
    format!(
        r#"
#include <metal_stdlib>
using namespace metal;

kernel void {name}(
    device {out_ty}*      y        [[buffer(0)]],
    device const uint*    packed   [[buffer(1)]],
    device const {ty}*    scales   [[buffer(2)]],
    device const {ty}*    biases   [[buffer(3)]],
    device const half*    x        [[buffer(4)]],
    constant uint&        n_rows   [[buffer(5)]],
    constant uint&        n_cols   [[buffer(6)]],
    constant uint&        group    [[buffer(7)]],
    constant uint&        n_tokens [[buffer(8)]],
    uint2 tgid [[threadgroup_position_in_grid]],
    uint  sg   [[simdgroup_index_in_threadgroup]],
    uint  lane [[thread_index_in_simdgroup]])
{{
    const uint row0 = tgid.x * {rows}u + sg * {per_simd}u;
    if (row0 >= n_rows) {{ return; }}
    const uint tok0 = tgid.y * {tile}u;
    if (tok0 >= n_tokens) {{ return; }}
    const uint tail = min({tile}u, n_tokens - tok0);
    const uint rows_here = min({per_simd}u, n_rows - row0);

    const uint words_per_row  = n_cols / 8u;
    const uint groups_per_row = n_cols / group;

    // Wiersz aktywacji na pozycję kafla. Poza ogonem wskazuje na token ostatni,
    // a nie na nic: liczymy dla niego śmieci, których nigdy nie zapisujemy.
    // Alternatywą jest warunek w pętli, a warunek robi z liczby przebiegów
    // wielkość zmienną — i wtedy `acc` przestaje być rejestrami. Zmierzone:
    // 1351 us wobec 294 us dla tej samej pracy przy T=1.
    device const half* xp[{tile}];
    for (uint t = 0; t < {tile}u; ++t) {{
        xp[t] = x + min(tok0 + t, n_tokens - 1u) * n_cols;
    }}

    float acc[{per_simd}][{tile}];
    for (uint r = 0; r < {per_simd}u; ++r) {{
        for (uint t = 0; t < {tile}u; ++t) {{ acc[r][t] = 0.0f; }}
    }}

    for (uint word = lane; word < words_per_row; word += 32u) {{
        const uint col0 = word * 8u;
        const uint g    = col0 / group;

        // Wagi wszystkich obsługiwanych wierszy rozpakowane RAZ, zanim ruszą
        // aktywacje: to one mają być tym, co zostaje w rejestrach, gdy jeden
        // odczyt aktywacji karmi {per_simd} mnożeń zamiast jednego.
        float wv[{per_simd}][8];
        for (uint r = 0; r < {per_simd}u; ++r) {{
            const uint rr = min(row0 + r, n_rows - 1u);
            const uint bits = packed[rr * words_per_row + word];
            const float sc  = float(scales[rr * groups_per_row + g]);
            const float bi  = float(biases[rr * groups_per_row + g]);
            for (uint j = 0; j < 8u; ++j) {{
                wv[r][j] = fma(float((bits >> (j * 4u)) & 0xFu), sc, bi);
            }}
        }}

        for (uint t = 0; t < {tile}u; ++t) {{
            device const half* xr = xp[t] + col0;
            for (uint j = 0; j < 8u; ++j) {{
                const float xv = float(xr[j]);
                for (uint r = 0; r < {per_simd}u; ++r) {{
                    acc[r][t] = fma(xv, wv[r][j], acc[r][t]);
                }}
            }}
        }}
    }}

    for (uint r = 0; r < rows_here; ++r) {{
        for (uint t = 0; t < tail; ++t) {{
            const float total = simd_sum(acc[r][t]);
            if (lane == 0u) {{ y[(tok0 + t) * n_rows + row0 + r] = {out_ty}(total); }}
        }}
    }}
}}
"#,
        name = name,
        ty = ty,
        out_ty = out_ty,
        rows = QMM_ROWS_PER_GROUP,
        per_simd = QMM_ROWS_PER_SIMD,
        tile = QMM_TILE,
    )
}

/// Output block of the matrix-unit form: tokens, rows and depth per threadgroup.
///
/// Swept, not chosen: at a full prefill chunk 32x64x32 costs 39.6 us per token
/// against 50.0 for 32x32x32, 41.3 for 64x32x32 and 47.0 for 64x64x32. Wider
/// than 64 rows stops paying and starts costing threadgroup memory.
pub const QMG_BM: u32 = 64;
pub const QMG_BN: u32 = 64;
pub const QMG_BK: u32 = 32;
/// Block of the token axis, which is the batch a caller must reach to use this.
pub const QMG_BLOCK: u32 = QMG_BM;

/// How the SIMD groups divide the block: tokens by rows.
///
/// Also swept. Two groups instead of four load fewer fragments per multiply but
/// lose more to having less to run: 49.4 us per token against 39.2.
pub const QMG_SG_M: u32 = 2;
pub const QMG_SG_N: u32 = 2;
pub const QMG_THREADS: u32 = QMG_SG_M * QMG_SG_N * 32;

pub fn qmg_affine_4bit_name(scales: ScaleDtype, out: OutDtype) -> String {
    format!(
        "qmg_affine_4bit_m{}n{}k{}_{}_out{}",
        QMG_BM,
        QMG_BN,
        QMG_BK,
        scales.suffix(),
        out.suffix()
    )
}

/// Grid for the matrix-unit form.
pub fn qmg_affine_4bit_groups(n_rows: u32, n_tokens: u32) -> (u32, u32) {
    (n_rows.div_ceil(QMG_BN), n_tokens.div_ceil(QMG_BM))
}

/// Whether a shape can use the matrix-unit form at all.
pub fn qmg_fits(n_rows: u32, n_cols: u32) -> bool {
    n_rows % QMG_BN == 0 && n_cols % QMG_BK == 0
}

/// Fused dequantize + matrix product on the SIMD matrix units.
///
/// The weight block is dequantized ONCE into threadgroup memory and then feeds
/// every token fragment in the block. That is what makes unpacking affordable:
/// against a block of tokens, the cost of turning nibbles into halves is
/// divided by the block instead of being paid per token.
///
/// The accumulation order is NOT the one the vector form uses — the matrix unit
/// sums its own way — so this kernel is not bit-comparable with decode. It is
/// held to the same numerical gate instead: no further from an f64 truth than
/// MLX itself.
pub fn qmg_affine_4bit_source(scales: ScaleDtype, out: OutDtype) -> String {
    let ty = scales.msl();
    let out_ty = out.msl();
    let name = qmg_affine_4bit_name(scales, out);
    let (fm, fn_) = (QMG_BM / QMG_SG_M / 8, QMG_BN / QMG_SG_N / 8);
    let stage_halfs = 2 * (QMG_BM * QMG_BK + QMG_BK * QMG_BN);

    // Wynik przechodzi przez pamiec grupy roboczej, w obu wariantach wyjscia.
    // Zapis wprost z jednostki macierzowej probowano: `simdgroup_store` do
    // pamieci urzadzenia rozrzuca osiem wierszy co `n_rows`, i mimo mniejszego
    // zuzycia pamieci grupy roboczej wychodzi wolniej (40,9 wobec 39,2 us na
    // token). Half i tak nie umie tam trafic, bo cel musi byc typu akumulatora.
    let floats = stage_halfs.div_ceil(2).max(QMG_BM * QMG_BN);
    let epilogue = format!(
        "    threadgroup_barrier(mem_flags::mem_threadgroup);\n\
         \x20   for (uint i = 0; i < {fm}u; ++i) {{\n\
         \x20       for (uint j = 0; j < {fn}u; ++j) {{\n\
         \x20           simdgroup_store(\n\
         \x20               acc[i][j], cs + (sg_tok + i * 8u) * {bn}u + sg_row + j * 8u, {bn}u);\n\
         \x20       }}\n\
         \x20   }}\n\
         \x20   threadgroup_barrier(mem_flags::mem_threadgroup);\n\
         \x20   for (uint i = tid; i < {bm}u * {bn}u; i += {threads}u) {{\n\
         \x20       const uint t = tok0 + i / {bn}u;\n\
         \x20       if (t < n_tokens) {{ y[t * n_rows + row0 + i % {bn}u] = {out_ty}(cs[i]); }}\n\
         \x20   }}",
        fm = fm,
        fn = fn_,
        bm = QMG_BM,
        bn = QMG_BN,
        threads = QMG_THREADS,
        out_ty = out_ty
    );

    format!(
        r#"
#include <metal_stdlib>
#include <metal_simdgroup_matrix>
using namespace metal;

kernel void {name}(
    device {out_ty}*      y        [[buffer(0)]],
    device const uint*    packed   [[buffer(1)]],
    device const {ty}*    scales   [[buffer(2)]],
    device const {ty}*    biases   [[buffer(3)]],
    device const half*    x        [[buffer(4)]],
    constant uint&        n_rows   [[buffer(5)]],
    constant uint&        n_cols   [[buffer(6)]],
    constant uint&        group    [[buffer(7)]],
    constant uint&        n_tokens [[buffer(8)]],
    uint2 tgid [[threadgroup_position_in_grid]],
    uint  sg   [[simdgroup_index_in_threadgroup]],
    uint  lane [[thread_index_in_simdgroup]])
{{
    const uint tid  = sg * 32u + lane;
    const uint row0 = tgid.x * {bn}u;
    const uint tok0 = tgid.y * {bm}u;

    const uint words_per_row  = n_cols / 8u;
    const uint groups_per_row = n_cols / group;

    // Jedna tablica na wszystko: w petli po K trzyma aktywacje i rozpakowane
    // wagi, a po niej — o ile wyjscie tego wymaga — wynik bloku w f32.
    threadgroup float cs[{floats}u];
    threadgroup half* xs = (threadgroup half*)cs;
    threadgroup half* ws = xs + 2u * {bm}u * {bk}u;

    // Wycinek bloku na grupe SIMD.
    const uint sg_tok = (sg / {sg_n}u) * {sub_bm}u;
    const uint sg_row = (sg % {sg_n}u) * {sub_bn}u;
    simdgroup_matrix<float, 8, 8> acc[{fm}][{fn}];
    for (uint i = 0; i < {fm}u; ++i) {{
        for (uint j = 0; j < {fn}u; ++j) {{ acc[i][j] = simdgroup_matrix<float, 8, 8>(0); }}
    }}

    // Potokowanie: nastepny blok jest czytany z pamieci urzadzenia i
    // rozpakowywany DO REJESTROW w czasie, gdy jednostki macierzowe licza
    // biezacy. Bez tego kazdy krok po K zaczyna sie od czekania na pamiec przy
    // bezczynnych jednostkach. Bufor wspoldzielony zostaje jeden — przeplot
    // siedzi w rejestrach, nie w drugiej kopii pamieci grupy roboczej.
    const uint xs_per_thread = {bm}u * {bk}u / {threads}u;
    const uint ws_per_thread = {bn}u * {bk}u / 8u / {threads}u;
    half  x_pre[{xs_pt}];
    half  w_pre[{ws_pt} * 8];

    // Dwa komplety buforow w pamieci grupy roboczej, na przemian. Przy jednym
    // trzeba bariery PRZED zapisem (zeby nikt jeszcze nie czytal) i PO nim (zeby
    // wszyscy zobaczyli) — dwie na krok po K. Przy dwoch zapis idzie tam, skad
    // nikt teraz nie czyta, wiec zostaje jedna. Szczyt zuzycia pamieci grupy
    // roboczej sie nie zmienia, bo wyznacza go i tak epilog.
    const uint blocks = (n_cols + {bk}u - 1u) / {bk}u;

#define QMG_FETCH(K0)                                                          \
    for (uint e = 0; e < xs_per_thread; ++e) {{                                 \
        const uint i = tid + e * {threads}u;                                    \
        x_pre[e] = x[min(tok0 + i / {bk}u, n_tokens - 1u) * n_cols              \
                     + (K0) + i % {bk}u];                                       \
    }}                                                                          \
    for (uint e = 0; e < ws_per_thread; ++e) {{                                 \
        const uint p = tid + e * {threads}u;                                    \
        const uint n_local = p / ({bk}u / 8u);                                  \
        const uint w_local = p % ({bk}u / 8u);                                  \
        const uint rr   = row0 + n_local;                                       \
        const uint col  = (K0) + w_local * 8u;                                  \
        const uint bits = packed[rr * words_per_row + col / 8u];                \
        const half sc = half(scales[rr * groups_per_row + col / group]);        \
        const half bi = half(biases[rr * groups_per_row + col / group]);        \
        for (uint j = 0; j < 8u; ++j) {{                                        \
            w_pre[e * 8u + j] = fma(half((bits >> (j * 4u)) & 0xFu), sc, bi);   \
        }}                                                                      \
    }}

    QMG_FETCH(0u)

    for (uint blk = 0; blk < blocks; ++blk) {{
        threadgroup half* xcur = xs + (blk & 1u) * {bm}u * {bk}u;
        threadgroup half* wcur = ws + (blk & 1u) * {bk}u * {bn}u;

        for (uint e = 0; e < xs_per_thread; ++e) {{
            const uint i = tid + e * {threads}u;
            xcur[i] = x_pre[e];
        }}
        for (uint e = 0; e < ws_per_thread; ++e) {{
            const uint p = tid + e * {threads}u;
            const uint n_local = p / ({bk}u / 8u);
            const uint w_local = p % ({bk}u / 8u);
            for (uint j = 0; j < 8u; ++j) {{
                wcur[(w_local * 8u + j) * {bn}u + n_local] = w_pre[e * 8u + j];
            }}
        }}
        threadgroup_barrier(mem_flags::mem_threadgroup);

        // Nastepny blok do rejestrow — niezalezny od mnozen ponizej, wiec
        // sprzet ma czym zapelnic czas oczekiwania na pamiec.
        if (blk + 1u < blocks) {{
            const uint k_next = (blk + 1u) * {bk}u;
            QMG_FETCH(k_next)
        }}

        for (uint kk = 0; kk < {bk}u; kk += 8u) {{
            simdgroup_matrix<half, 8, 8> a[{fm}], b[{fn}];
            for (uint i = 0; i < {fm}u; ++i) {{
                simdgroup_load(a[i], xcur + (sg_tok + i * 8u) * {bk}u + kk, {bk}u);
            }}
            for (uint j = 0; j < {fn}u; ++j) {{
                simdgroup_load(b[j], wcur + kk * {bn}u + sg_row + j * 8u, {bn}u);
            }}
            for (uint i = 0; i < {fm}u; ++i) {{
                for (uint j = 0; j < {fn}u; ++j) {{
                    simdgroup_multiply_accumulate(acc[i][j], a[i], b[j], acc[i][j]);
                }}
            }}
        }}
    }}
#undef QMG_FETCH

{epilogue}
}}
"#,
        name = name,
        ty = ty,
        out_ty = out_ty,
        bm = QMG_BM,
        bn = QMG_BN,
        bk = QMG_BK,
        sg_n = QMG_SG_N,
        sub_bm = QMG_BM / QMG_SG_M,
        sub_bn = QMG_BN / QMG_SG_N,
        fm = fm,
        fn = fn_,
        xs_pt = QMG_BM * QMG_BK / QMG_THREADS,
        ws_pt = QMG_BN * QMG_BK / 8 / QMG_THREADS,
        floats = floats,
        threads = QMG_THREADS,
        epilogue = epilogue,
    )
}

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

kernel void silu_mul_f32_f16(
    device half*         out   [[buffer(0)]],
    device const float*  gate  [[buffer(1)]],
    device const float*  up    [[buffer(2)]],
    constant uint&       n     [[buffer(3)]],
    uint gid [[thread_position_in_grid]])
{
    if (gid >= n) { return; }
    const float g = gate[gid];
    out[gid] = half(g / (1.0f + exp(-g)) * up[gid]);
}
"#;

pub const SILU_MUL_NAME: &str = "silu_mul_f32_f16";

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
    if (token >= n_tokens || seq > seq_cap || seq_cap > {max_seq}u) {{ return; }}
    const uint kv_head = head / (n_heads / n_kv_heads);

    // Przyczynowosc bez maski: zapytanie kafla siedzi na pozycji
    // `seq - n_tokens + token`, wiec po prostu konczy petle na swojej wlasnej.
    // Maska dolozylaby minus nieskonczonosci, ktore i tak trzeba pominac.
    const uint len = seq - n_tokens + token + 1u;

    threadgroup float scores[{max_seq}];
    threadgroup float reduce[{simdgroups}];

    device const half* qh = q + (token * n_heads + head) * dim;
    device const half* kh = k + kv_head * seq_cap * dim;
    device const half* vh = v + kv_head * seq_cap * dim;

    // Faza 1: iloczyn skalarny zapytania z kazdym kluczem.
    for (uint j = tid; j < len; j += {threads}u) {{
        float acc = 0.0f;
        device const half* kj = kh + j * dim;
        for (uint c = 0; c < dim; ++c) {{
            acc = fma(float(qh[c]), float(kj[c]), acc);
        }}
        scores[j] = acc * scale;
    }}
    threadgroup_barrier(mem_flags::mem_threadgroup);

    // Faza 2: maksimum, potem wykladniki i ich suma. Odjecie maksimum przed
    // exp jest tu warunkiem poprawnosci, nie ostroznoscia: bez niego dlugi
    // kontekst przelewa f32 i softmax daje NaN.
    float local = -INFINITY;
    for (uint j = tid; j < len; j += {threads}u) {{ local = max(local, scores[j]); }}
    local = simd_max(local);
    if (lane == 0u) {{ reduce[sg] = local; }}
    threadgroup_barrier(mem_flags::mem_threadgroup);
    float m = reduce[0];
    for (uint g = 1u; g < {simdgroups}u; ++g) {{ m = max(m, reduce[g]); }}

    float partial = 0.0f;
    for (uint j = tid; j < len; j += {threads}u) {{
        const float e = exp(scores[j] - m);
        scores[j] = e;
        partial += e;
    }}
    partial = simd_sum(partial);
    if (lane == 0u) {{ reduce[sg] = partial; }}
    threadgroup_barrier(mem_flags::mem_threadgroup);
    float total = reduce[0];
    for (uint g = 1u; g < {simdgroups}u; ++g) {{ total += reduce[g]; }}

    // Faza 3: jeden watek na kanal wyjscia.
    if (tid < dim) {{
        float acc = 0.0f;
        for (uint j = 0; j < len; ++j) {{
            acc = fma(scores[j], float(vh[j * dim + tid]), acc);
        }}
        out[(token * n_heads + head) * dim + tid] = half(acc / total);
    }}
}}
"#,
        name = name,
        dim = head_dim,
        max_seq = ATTN_MAX_SEQ,
        threads = ATTN_THREADS,
        simdgroups = ATTN_THREADS / 32,
    )
}

/// One threadgroup per (token, head). Derived so a caller cannot size the grid
/// for a batch the kernel was not given.
pub fn attn_groups(heads: u32, tokens: u32) -> u32 {
    heads * tokens
}

pub fn attn_decode_name(head_dim: u32) -> String {
    format!("attn_decode_d{head_dim}_s{ATTN_MAX_SEQ}")
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
            qmv_affine_4bit_name(ScaleDtype::Bf16, OutDtype::F32),
            "qmv_affine_4bit_r4_bf16_outf32"
        );
        assert_eq!(
            qmv_affine_4bit_name(ScaleDtype::F16, OutDtype::F16),
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
        assert_eq!(attn_decode_name(128), "attn_decode_d128_s2048");
        assert!(src.contains("attn_decode_d128_s2048"));
        // Limit dlugosci cache'u jest w nazwie, w deklaracji tablicy i w
        // warunku odmowy — rozjazd któregokolwiek z nich to odczyt poza bufor.
        assert!(src.contains(&format!("threadgroup float scores[{ATTN_MAX_SEQ}]")));
        assert!(src.contains(&format!("seq_cap > {ATTN_MAX_SEQ}u")));
        // Krok składowania liczy się z POJEMNOŚCI, nie z bieżącej długości.
        assert!(src.contains("kv_head * seq_cap * dim"));
        assert!(!src.contains("kv_head * seq * dim"));
        assert!(src.contains("head / (n_heads / n_kv_heads)"));
    }

    #[test]
    fn output_type_is_a_parameter_not_a_second_family() {
        let a = qmv_affine_4bit_source(ScaleDtype::Bf16, OutDtype::F32);
        let b = qmv_affine_4bit_source(ScaleDtype::Bf16, OutDtype::F16);
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
            .replace(&qmv_affine_4bit_name(ScaleDtype::Bf16, out), "ENTRY")
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
        let f16 = qmv_affine_4bit_source(ScaleDtype::F16, OutDtype::F32);
        let bf16 = qmv_affine_4bit_source(ScaleDtype::Bf16, OutDtype::F32);
        assert!(f16.contains("device const half*    scales"));
        assert!(bf16.contains("device const bfloat*    scales"));
        // Poza nazwą i typem skal źródła muszą być identyczne: dwie ręcznie
        // pisane kopie rozjeżdżają się przy pierwszej poprawce.
        let norm = |s: &str, ty: &str, name: &str| {
            s.replace(ty, "SCALE_T").replace(name, "ENTRY")
        };
        assert_eq!(
            norm(&f16, "half*    scales", &qmv_affine_4bit_name(ScaleDtype::F16, OutDtype::F32))
                .replace("half*    biases", "SCALE_T biases")
                .replace("const half* s", "const SCALE_T s")
                .replace("const half* b", "const SCALE_T b"),
            norm(
                &bf16,
                "bfloat*    scales",
                &qmv_affine_4bit_name(ScaleDtype::Bf16, OutDtype::F32)
            )
            .replace("bfloat*    biases", "SCALE_T biases")
            .replace("const bfloat* s", "const SCALE_T s")
            .replace("const bfloat* b", "const SCALE_T b")
        );
    }
}
