// ===== File: k_quants.rs — direct Metal kernels for compact GGUF K-quants =====

use super::{
    OutDtype, QMG_BK, QMG_BM, QMG_BN, QMG_SG_M, QMG_SG_N, QMG_THREADS, QMM_ROWS_PER_GROUP,
    QMM_ROWS_PER_SIMD, QMM_TILE,
};

pub fn q4_k_qmv_name(out: OutDtype) -> String {
    format!("qmv_q4_k_out{}", out.suffix())
}

pub fn q4_k_qmm_name(out: OutDtype) -> String {
    format!(
        "qmm_q4_k_r{}s{}t{}_out{}",
        QMM_ROWS_PER_GROUP,
        QMM_ROWS_PER_SIMD,
        QMM_TILE,
        out.suffix()
    )
}

pub fn q4_k_qmv_groups(rows: u32) -> u32 {
    rows.div_ceil(4)
}

pub const Q4_K_EMBED_NAME: &str = "embed_q4_k";

pub fn q4_k_embed_source() -> String {
    let value = q4_k_value_source();
    format!(
        r#"
#include <metal_stdlib>
using namespace metal;

{value}

kernel void {name}(
    device half* out [[buffer(0)]],
    device const uchar* blocks [[buffer(1)]],
    device const uint* tokens [[buffer(2)]],
    constant uint& hidden [[buffer(3)]],
    constant uint& n_tokens [[buffer(4)]],
    uint gid [[thread_position_in_grid]])
{{
    const uint total = hidden * n_tokens;
    if (gid >= total) {{ return; }}
    const uint row = gid / hidden;
    const uint col = gid % hidden;
    const uint token = tokens[row];
    out[gid] = half(q4_k_value(blocks, token, col, hidden / 256u));
}}
"#,
        value = value,
        name = Q4_K_EMBED_NAME,
    )
}

pub fn q4_k_qmm_groups(rows: u32, tokens: u32) -> (u32, u32) {
    (rows.div_ceil(QMM_ROWS_PER_GROUP), tokens.div_ceil(QMM_TILE))
}

pub fn q4_k_qmg_name(out: OutDtype) -> String {
    format!(
        "qmg_q4_k_m{}n{}k{}_out{}",
        QMG_BM,
        QMG_BN,
        QMG_BK,
        out.suffix()
    )
}

pub fn q4_k_qmg_groups(rows: u32, tokens: u32) -> (u32, u32) {
    (rows.div_ceil(QMG_BN), tokens.div_ceil(QMG_BM))
}

pub fn q4_k_qmv_source(out: OutDtype) -> String {
    let out_ty = out.msl();
    let name = q4_k_qmv_name(out);
    format!(
        r#"
#include <metal_stdlib>
using namespace metal;

kernel void {name}(
    device {out_ty}* y [[buffer(0)]],
    device const uchar* blocks [[buffer(1)]],
    device const half* x [[buffer(2)]],
    constant uint& n_rows [[buffer(3)]],
    constant uint& n_cols [[buffer(4)]],
    uint tgid [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]])
{{
    const uint row = tgid * 4u + sg;
    if (row >= n_rows) {{ return; }}
    const uint superblocks = n_cols / 256u;
    float acc = 0.0f;
    for (uint sb = 0; sb < superblocks; ++sb) {{
        device const uchar* b = blocks + (row * superblocks + sb) * 144u;
        const ushort d_bits = ushort(b[0]) | (ushort(b[1]) << 8u);
        const ushort dmin_bits = ushort(b[2]) | (ushort(b[3]) << 8u);
        const float d = float(as_type<half>(d_bits));
        const float dmin = float(as_type<half>(dmin_bits));
        const uint sub = lane / 4u;
        const uint sub_base = (sub / 2u) * 32u;
        const float sc = (sub < 4u)
            ? d * float(b[4u + sub] & 63u)
            : d * float((b[8u + sub] & 15u) | ((b[4u + sub - 4u] >> 6u) << 4u));
        const float mn = (sub < 4u)
            ? dmin * float(b[8u + sub] & 63u)
            : dmin * float((b[8u + sub] >> 4u) | ((b[4u + sub] >> 6u) << 4u));
        for (uint j = 0; j < 8u; ++j) {{
            const uint e = lane * 8u + j;
            const uchar qbyte = b[16u + sub_base + (lane % 4u) * 8u + j];
            const float q = float((sub & 1u) == 0u ? qbyte & 0x0Fu : qbyte >> 4u);
            acc += float(x[sb * 256u + e]) * fma(q, sc, -mn);
        }}
    }}
    const float total = simd_sum(acc);
    if (lane == 0u) {{ y[row] = {out_ty}(total); }}
}}
"#,
        name = name,
        out_ty = out_ty,
    )
}

fn q4_k_value_source() -> &'static str {
    r#"
inline float q4_k_value(
    device const uchar* blocks,
    uint row,
    uint col,
    uint superblocks)
{
    const uint sb = col / 256u;
    const uint e = col % 256u;
    device const uchar* b = blocks + (row * superblocks + sb) * 144u;
    const ushort d_bits = ushort(b[0]) | (ushort(b[1]) << 8u);
    const ushort dmin_bits = ushort(b[2]) | (ushort(b[3]) << 8u);
    const float d = float(as_type<half>(d_bits));
    const float dmin = float(as_type<half>(dmin_bits));
    const uint sub = e / 32u;
    const uint within = e % 32u;
    const uchar qbyte = b[16u + (sub / 2u) * 32u + within];
    const float q = float((sub & 1u) == 0u ? qbyte & 0x0Fu : qbyte >> 4u);
    const float sc = (sub < 4u)
        ? d * float(b[4u + sub] & 63u)
        : d * float((b[8u + sub] & 15u) | ((b[4u + sub - 4u] >> 6u) << 4u));
    const float mn = (sub < 4u)
        ? dmin * float(b[8u + sub] & 63u)
        : dmin * float((b[8u + sub] >> 4u) | ((b[4u + sub] >> 6u) << 4u));
    return fma(q, sc, -mn);
}
"#
}

pub fn q4_k_qmm_source(out: OutDtype) -> String {
    let out_ty = out.msl();
    let name = q4_k_qmm_name(out);
    let value = q4_k_value_source();
    format!(
        r#"
#include <metal_stdlib>
using namespace metal;

{value}

kernel void {name}(
    device {out_ty}* y [[buffer(0)]],
    device const uchar* blocks [[buffer(1)]],
    device const half* x [[buffer(2)]],
    constant uint& n_rows [[buffer(3)]],
    constant uint& n_cols [[buffer(4)]],
    constant uint& n_tokens [[buffer(5)]],
    uint2 tgid [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]])
{{
    const uint row0 = tgid.x * {rows}u + sg * {per_simd}u;
    if (row0 >= n_rows) {{ return; }}
    const uint tok0 = tgid.y * {tile}u;
    if (tok0 >= n_tokens) {{ return; }}
    const uint tail = min({tile}u, n_tokens - tok0);
    const uint rows_here = min({per_simd}u, n_rows - row0);
    const uint words_per_row = n_cols / 8u;
    const uint superblocks = n_cols / 256u;

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
        float wv[{per_simd}][8];
        for (uint r = 0; r < {per_simd}u; ++r) {{
            const uint row = min(row0 + r, n_rows - 1u);
            for (uint j = 0; j < 8u; ++j) {{
                wv[r][j] = q4_k_value(blocks, row, col0 + j, superblocks);
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
        value = value,
        name = name,
        out_ty = out_ty,
        rows = QMM_ROWS_PER_GROUP,
        per_simd = QMM_ROWS_PER_SIMD,
        tile = QMM_TILE,
    )
}

pub fn q4_k_qmg_source(out: OutDtype) -> String {
    let out_ty = out.msl();
    let name = q4_k_qmg_name(out);
    let value = q4_k_value_source();
    let (fm, fn_) = (QMG_BM / QMG_SG_M / 8, QMG_BN / QMG_SG_N / 8);
    let stage_halfs = 2 * (QMG_BM * QMG_BK + QMG_BK * QMG_BN);
    let floats = stage_halfs.div_ceil(2).max(QMG_BM * QMG_BN);
    let epilogue = format!(
        "    threadgroup_barrier(mem_flags::mem_threadgroup);\n\
         \x20   for (uint i = 0; i < {fm}u; ++i) {{\n\
         \x20       for (uint j = 0; j < {fn}u; ++j) {{\n\
         \x20           simdgroup_store(acc[i][j], cs + (sg_tok + i * 8u) * {bn}u + sg_row + j * 8u, {bn}u);\n\
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
        out_ty = out_ty,
    );
    format!(
        r#"
#include <metal_stdlib>
#include <metal_simdgroup_matrix>
using namespace metal;

{value}

kernel void {name}(
    device {out_ty}* y [[buffer(0)]],
    device const uchar* blocks [[buffer(1)]],
    device const half* x [[buffer(2)]],
    constant uint& n_rows [[buffer(3)]],
    constant uint& n_cols [[buffer(4)]],
    constant uint& n_tokens [[buffer(5)]],
    uint2 tgid [[threadgroup_position_in_grid]],
    uint sg [[simdgroup_index_in_threadgroup]],
    uint lane [[thread_index_in_simdgroup]])
{{
    const uint tid = sg * 32u + lane;
    const uint row0 = tgid.x * {bn}u;
    const uint tok0 = tgid.y * {bm}u;
    const uint superblocks = n_cols / 256u;
    const uint sg_tok = (sg / {sg_n}u) * {sub_bm}u;
    const uint sg_row = (sg % {sg_n}u) * {sub_bn}u;
    simdgroup_matrix<float, 8, 8> acc[{fm}][{fn}];
    for (uint i = 0; i < {fm}u; ++i) {{
        for (uint j = 0; j < {fn}u; ++j) {{ acc[i][j] = simdgroup_matrix<float, 8, 8>(0); }}
    }}

    threadgroup float cs[{floats}u];
    threadgroup half* xs = (threadgroup half*)cs;
    threadgroup half* ws = xs + 2u * {bm}u * {bk}u;
    const uint xs_per_thread = {bm}u * {bk}u / {threads}u;
    const uint ws_per_thread = {bn}u * {bk}u / 8u / {threads}u;
    half x_pre[{xs_pt}];
    half w_pre[{ws_pt} * 8];
    const uint k_blocks = (n_cols + {bk}u - 1u) / {bk}u;

#define K_FETCH(K0)                                                            \
    for (uint e = 0; e < xs_per_thread; ++e) {{                                \
        const uint i = tid + e * {threads}u;                                   \
        x_pre[e] = x[min(tok0 + i / {bk}u, n_tokens - 1u) * n_cols             \
                     + (K0) + i % {bk}u];                                      \
    }}                                                                         \
    for (uint e = 0; e < ws_per_thread; ++e) {{                                \
        const uint p = tid + e * {threads}u;                                   \
        const uint n_local = p / ({bk}u / 8u);                                 \
        const uint w_local = p % ({bk}u / 8u);                                 \
        const uint rr = row0 + n_local;                                        \
        const uint k0 = (K0);                                                   \
        const uint sb = k0 / 256u;                                             \
        const uint sub = (k0 / 32u) & 7u;                                      \
        device const uchar* b = blocks + (rr * superblocks + sb) * 144u;       \
        const ushort d_bits = ushort(b[0]) | (ushort(b[1]) << 8u);             \
        const ushort dmin_bits = ushort(b[2]) | (ushort(b[3]) << 8u);           \
        const float d = float(as_type<half>(d_bits));                          \
        const float dmin = float(as_type<half>(dmin_bits));                    \
        const float sc = (sub < 4u)                                         \
            ? d * float(b[4u + sub] & 63u)                                  \
            : d * float((b[8u + sub] & 15u) | ((b[4u + sub - 4u] >> 6u) << 4u)); \
        const float mn = (sub < 4u)                                         \
            ? dmin * float(b[8u + sub] & 63u)                              \
            : dmin * float((b[8u + sub] >> 4u) | ((b[4u + sub] >> 6u) << 4u)); \
        for (uint j = 0; j < 8u; ++j) {{                                       \
            const uchar qbyte = b[16u + (sub / 2u) * 32u + w_local * 8u + j]; \
            const float q = float((sub & 1u) == 0u ? qbyte & 0x0Fu : qbyte >> 4u); \
            w_pre[e * 8u + j] = half(fma(q, sc, -mn));                        \
        }}                                                                      \
    }}

    K_FETCH(0u)
    for (uint blk = 0; blk < k_blocks; ++blk) {{
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
        if (blk + 1u < k_blocks) {{
            K_FETCH((blk + 1u) * {bk}u)
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
#undef K_FETCH

{epilogue}
}}
"#,
        value = value,
        name = name,
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
