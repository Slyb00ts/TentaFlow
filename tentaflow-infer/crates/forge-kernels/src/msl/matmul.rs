// ===== File: matmul.rs — the quantized matrix family, three forms of one op =====
//
// Vector, register-blocked and matrix-unit. They differ in how much of the
// problem one threadgroup carries, not in what they compute: every one of them
// is `y = x * (q * scale + bias)`. Which serves which shape is decided by
// `variant.rs`, with the measurement beside each entry.
//
// The code width is a parameter of the family for the same reason: four and six
// bits compute the same formula and differ only in where the bits are.

use super::{Bits, OutDtype, ScaleDtype};

/// Output rows handled by one threadgroup: one SIMD group per row.
pub const QMV_ROWS_PER_GROUP: u32 = 4;
/// Four SIMD groups of 32 lanes.
pub const QMV_THREADS: u32 = QMV_ROWS_PER_GROUP * 32;

/// Entry-point name for a given scale type and output type.
pub fn qmv_affine_name(bits: Bits, scales: ScaleDtype, out: OutDtype) -> String {
    format!(
        "qmv_affine_{}_r{}_{}_out{}",
        bits.suffix(),
        QMV_ROWS_PER_GROUP,
        scales.suffix(),
        out.suffix()
    )
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
pub fn qmv_affine_source(bits: Bits, scales: ScaleDtype, out: OutDtype) -> String {
    let ty = scales.msl();
    let out_ty = out.msl();
    let name = qmv_affine_name(bits, scales, out);
    let high_param = bits.high_param(5);
    let high_row = bits.high_row("row");
    let high_word = bits.high_word("col0");
    let code = bits.code("j");
    // Sześciobitowa odmiana dokłada bufor, więc skalary przesuwają się o jeden.
    let (b_rows, b_cols, b_group) = match bits {
        Bits::Four => (5, 6, 7),
        Bits::Six => (6, 7, 8),
    };
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
{high_param}    constant uint&        n_rows   [[buffer({b_rows})]],
    constant uint&        n_cols   [[buffer({b_cols})]],
    constant uint&        group    [[buffer({b_group})]],
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
{high_row}
    float acc = 0.0f;
    // Kolejne linie czytaja kolejne slowa, wiec odczyt jest sklejony.
    for (uint word = lane; word < words_per_row; word += 32u) {{
        const uint bits = w[word];
        const uint col0 = word * 8u;
        const uint g    = col0 / group;
        const float sc  = float(s[g]);
        const float bi  = float(b[g]);
        {high_word}
        for (uint j = 0; j < 8u; ++j) {{
            const float q = float({code});
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
        high_param = high_param,
        high_row = high_row,
        high_word = high_word,
        code = code,
        b_rows = b_rows,
        b_cols = b_cols,
        b_group = b_group,
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
pub fn qmm_affine_name(bits: Bits, scales: ScaleDtype, out: OutDtype) -> String {
    format!(
        "qmm_affine_{}_r{}s{}t{}_{}_out{}",
        bits.suffix(),
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
pub fn qmm_affine_source(bits: Bits, scales: ScaleDtype, out: OutDtype) -> String {
    let high_param = bits.high_param(5);
    let high_word = bits.high_word_at("rr", "col0");
    let code = bits.code("j");
    let (b_rows, b_cols, b_group, b_tokens) = match bits {
        Bits::Four => (5, 6, 7, 8),
        Bits::Six => (6, 7, 8, 9),
    };
    let ty = scales.msl();
    let out_ty = out.msl();
    let name = qmm_affine_name(bits, scales, out);
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
{high_param}    constant uint&        n_rows   [[buffer({b_rows})]],
    constant uint&        n_cols   [[buffer({b_cols})]],
    constant uint&        group    [[buffer({b_group})]],
    constant uint&        n_tokens [[buffer({b_tokens})]],
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
            {high_word}
            for (uint j = 0; j < 8u; ++j) {{
                wv[r][j] = fma(float({code}), sc, bi);
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
        high_param = high_param,
        high_word = high_word,
        code = code,
        b_rows = b_rows,
        b_cols = b_cols,
        b_group = b_group,
        b_tokens = b_tokens,
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

pub fn qmg_affine_name(bits: Bits, scales: ScaleDtype, out: OutDtype) -> String {
    format!(
        "qmg_affine_{}_m{}n{}k{}_{}_out{}",
        bits.suffix(),
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
pub fn qmg_affine_source(bits: Bits, scales: ScaleDtype, out: OutDtype) -> String {
    let high_param = bits.high_param(5);
    let qmg_high = bits.high_word_at("rr", "col");
    let qmg_code = bits.code("j");
    let (b_rows, b_cols, b_group, b_tokens) = match bits {
        Bits::Four => (5, 6, 7, 8),
        Bits::Six => (6, 7, 8, 9),
    };
    let ty = scales.msl();
    let out_ty = out.msl();
    let name = qmg_affine_name(bits, scales, out);
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
{high_param}    constant uint&        n_rows   [[buffer({b_rows})]],
    constant uint&        n_cols   [[buffer({b_cols})]],
    constant uint&        group    [[buffer({b_group})]],
    constant uint&        n_tokens [[buffer({b_tokens})]],
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
        {qmg_high}                                                              \
        for (uint j = 0; j < 8u; ++j) {{                                        \
            w_pre[e * 8u + j] = fma(half({qmg_code}), sc, bi);                  \
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
        high_param = high_param,
        qmg_high = qmg_high,
        qmg_code = qmg_code,
        b_rows = b_rows,
        b_cols = b_cols,
        b_group = b_group,
        b_tokens = b_tokens,
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
