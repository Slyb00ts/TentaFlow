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

/// Entry-point name for a given scale type.
pub fn qmv_affine_4bit_name(scales: ScaleDtype) -> String {
    format!("qmv_affine_4bit_r4_{}", scales.suffix())
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
pub fn qmv_affine_4bit_source(scales: ScaleDtype) -> String {
    let ty = scales.msl();
    let name = qmv_affine_4bit_name(scales);
    format!(
        r#"
#include <metal_stdlib>
using namespace metal;

kernel void {name}(
    device float*         y        [[buffer(0)]],
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
    if (lane == 0u) {{ y[row] = total; }}
}}
"#,
        name = name,
        ty = ty,
        rows = QMV_ROWS_PER_GROUP,
    )
}

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
            qmv_affine_4bit_name(ScaleDtype::Bf16),
            "qmv_affine_4bit_r4_bf16"
        );
        assert_eq!(
            qmv_affine_4bit_name(ScaleDtype::F16),
            "qmv_affine_4bit_r4_f16"
        );
        assert_eq!(QMV_THREADS, QMV_ROWS_PER_GROUP * 32);
    }

    #[test]
    fn both_variants_come_from_one_template() {
        let f16 = qmv_affine_4bit_source(ScaleDtype::F16);
        let bf16 = qmv_affine_4bit_source(ScaleDtype::Bf16);
        assert!(f16.contains("device const half*    scales"));
        assert!(bf16.contains("device const bfloat*    scales"));
        // Poza nazwą i typem skal źródła muszą być identyczne: dwie ręcznie
        // pisane kopie rozjeżdżają się przy pierwszej poprawce.
        let norm = |s: &str, ty: &str, name: &str| {
            s.replace(ty, "SCALE_T").replace(name, "ENTRY")
        };
        assert_eq!(
            norm(&f16, "half*    scales", &qmv_affine_4bit_name(ScaleDtype::F16))
                .replace("half*    biases", "SCALE_T biases")
                .replace("const half* s", "const SCALE_T s")
                .replace("const half* b", "const SCALE_T b"),
            norm(
                &bf16,
                "bfloat*    scales",
                &qmv_affine_4bit_name(ScaleDtype::Bf16)
            )
            .replace("bfloat*    biases", "SCALE_T biases")
            .replace("const bfloat* s", "const SCALE_T s")
            .replace("const bfloat* b", "const SCALE_T b")
        );
    }
}
