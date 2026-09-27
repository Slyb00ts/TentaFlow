// ===== File: scatter.rs — rozrzut kolumn z ciągłego bufora ANE do wyjścia projekcji =====
//
// Neural Engine oddaje swój ogon wierszy jako CIĄGŁĄ macierz `[T_model, src_width]`
// f16 — model CoreML ma stały kształt i nie umie pisać z krokiem obcego bufora.
// Wyjście projekcji ma natomiast krok `rows` (pełną szerokość wagi) i typ, jaki
// wybrał wykonawca: f16 dla aktywacji, f32 dla tego, co trafia do rezyduum.
// Ten kernel przenosi okno kolumn z jednego do drugiego, dla `tokens` wierszy —
// nie dla wszystkich `T_model`, bo kafel bywa krótszy niż model.
//
// Typ wyjścia jest parametrem tej samej rodziny (§6.3), tak jak w `qmv`:
// jedna szablonowa funkcja, dwie nazwy. Kopiowanie po osiem elementów, bo
// szerokość okna jest wielokrotnością bloku `QMG_BN` (64), więc dzieli się
// przez osiem zawsze; ogon skalarny istnieje na wypadek wywołania z innym
// `count` i jest maskowany, a nie zabroniony.
//
// Przykład (host):
//   let src = msl::scatter_cols_source(OutDtype::F32);
//   let name = msl::scatter_cols_name(OutDtype::F32);   // "scatter_cols_f16_f32"
//   grid = (msl::scatter_groups(count, tokens), 1, 1), block = (SCATTER_THREADS, 1, 1)
//   args: src, dst, src_width, src_col0, dst_stride, dst_col0, count, tokens

use super::OutDtype;

/// Wątków w grupie roboczej kernela rozrzutu.
pub const SCATTER_THREADS: u32 = 256;

/// Elementów kopiowanych przez jeden wątek: dwa `half4` na wejściu, dwa
/// `half4`/`float4` na wyjściu.
pub const SCATTER_LANES: u32 = 8;

/// Nazwa punktu wejścia: źródło jest zawsze f16, wyjście według `out`.
pub fn scatter_cols_name(out: OutDtype) -> String {
    format!("scatter_cols_f16_{}", out.suffix())
}

/// Siatka jednowymiarowa: `tokens` wierszy po `ceil(count / 8)` wątków.
pub fn scatter_groups(count: u32, tokens: u32) -> u32 {
    let per_row = count.div_ceil(SCATTER_LANES);
    (per_row * tokens).div_ceil(SCATTER_THREADS).max(1)
}

/// `dst[t*dst_stride + dst_col0 + c] = src[t*src_width + src_col0 + c]`
/// dla `t < tokens`, `c < count`, z konwersją do typu wyjścia.
///
/// Ścieżka wektorowa wymaga, żeby początek okna w OBU buforach padał na
/// cztery elementy (8 B dla `half4`, 16 B dla `float4`); sprawdzane w kernelu
/// na indeksach, a nie zakładane, bo koszt to jedno porównanie na osiem
/// elementów, a nieprawidłowo wyrównany odczyt wektorowy nie jest błędem,
/// tylko innym wynikiem.
pub fn scatter_cols_source(out: OutDtype) -> String {
    let name = scatter_cols_name(out);
    let out_ty = out.msl();
    let out_vec = match out {
        OutDtype::F16 => "half4",
        OutDtype::F32 => "float4",
    };
    format!(
        r#"
#include <metal_stdlib>
using namespace metal;

kernel void {name}(
    device const half*    src        [[buffer(0)]],
    device {out_ty}*      dst        [[buffer(1)]],
    constant uint&        src_width  [[buffer(2)]],
    constant uint&        src_col0   [[buffer(3)]],
    constant uint&        dst_stride [[buffer(4)]],
    constant uint&        dst_col0   [[buffer(5)]],
    constant uint&        count      [[buffer(6)]],
    constant uint&        tokens     [[buffer(7)]],
    uint gid [[thread_position_in_grid]])
{{
    const uint lanes   = {lanes}u;
    const uint per_row = (count + lanes - 1u) / lanes;
    const uint t  = gid / per_row;
    const uint c0 = (gid - t * per_row) * lanes;
    if (t >= tokens || c0 >= count) {{ return; }}

    const uint s_off = t * src_width  + src_col0 + c0;
    const uint d_off = t * dst_stride + dst_col0 + c0;
    device const half* s = src + s_off;
    device {out_ty}*   d = dst + d_off;

    const bool whole   = (c0 + lanes) <= count;
    const bool aligned = ((s_off | d_off) & 3u) == 0u;
    if (whole && aligned) {{
        const half4 a = *((device const half4*)(s));
        const half4 b = *((device const half4*)(s + 4u));
        *((device {out_vec}*)(d))      = {out_vec}(a);
        *((device {out_vec}*)(d + 4u)) = {out_vec}(b);
        return;
    }}
    for (uint i = 0u; i < lanes && c0 + i < count; ++i) {{
        d[i] = {out_ty}(s[i]);
    }}
}}
"#,
        lanes = SCATTER_LANES,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_name_carries_both_element_types() {
        assert_eq!(scatter_cols_name(OutDtype::F16), "scatter_cols_f16_f16");
        assert_eq!(scatter_cols_name(OutDtype::F32), "scatter_cols_f16_f32");
        assert!(scatter_cols_source(OutDtype::F32).contains("scatter_cols_f16_f32"));
        assert!(scatter_cols_source(OutDtype::F32).contains("device float*      dst"));
        assert!(scatter_cols_source(OutDtype::F16).contains("device half*      dst"));
    }

    #[test]
    fn output_type_is_a_parameter_not_a_second_kernel() {
        // Poza typem wyjścia oba źródła są identyczne. Podmiana obejmuje
        // DOKŁADNIE miejsca, w których typ wyjścia występuje: deklarację,
        // dwa zapisy wektorowe i zapis skalarny. Podmiana samego „half" trafiłaby
        // też w źródło i test porównywałby dwa równie zniekształcone teksty.
        let strip = |out: OutDtype, ty: &str, vec: &str| {
            let stripped = scatter_cols_source(out)
                .replace(&scatter_cols_name(out), "ENTRY")
                .replace(&format!("device {ty}*      dst"), "device OUT_T*      dst")
                .replace(&format!("device {ty}*   d ="), "device OUT_T*   d =")
                .replace(&format!("(device {vec}*)"), "(device OUT_V*)")
                .replace(&format!("= {vec}(a)"), "= OUT_V(a)")
                .replace(&format!("= {vec}(b)"), "= OUT_V(b)")
                .replace(&format!("= {ty}(s[i])"), "= OUT_T(s[i])");
            assert!(
                stripped.contains("device OUT_T*      dst"),
                "podmiana nie trafiła"
            );
            assert!(stripped.contains("= OUT_V(b)"), "podmiana nie trafiła");
            assert!(stripped.contains("= OUT_T(s[i])"), "podmiana nie trafiła");
            stripped
        };
        assert_eq!(
            strip(OutDtype::F16, "half", "half4"),
            strip(OutDtype::F32, "float", "float4")
        );
    }

    #[test]
    fn the_grid_covers_every_token_and_a_ragged_width() {
        // 3072 kolumn to 384 wątki na wiersz; 256 wierszy to 98 304 wątków,
        // czyli 384 grupy po 256.
        assert_eq!(scatter_groups(3072, 256), 384);
        assert_eq!(scatter_groups(64, 1), 1);
        // 65 kolumn wymaga dziewięciu wątków na wiersz, nie ośmiu.
        assert_eq!(scatter_groups(65, 256), (9 * 256u32).div_ceil(256));
        assert_eq!(scatter_groups(0, 0), 1, "pusta siatka to nadal jedna grupa");
    }
}
