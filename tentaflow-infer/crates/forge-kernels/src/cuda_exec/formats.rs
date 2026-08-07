// ===== File: formats.rs — which kernel multiplies which quantization =====

use super::{CudaExec, Quantized};

use crate::launchers::Kernels;

use forge_hal::DevBuffer;
use forge_types::{ForgeError, QuantKind, Result};

/// Formaty, w których format wybiera WYŁĄCZNIE nazwę kernela.
///
/// Ta tabela jest przeniesiona z `forge-engine::model::quant_dispatch`, gdzie
/// powstała po tym, jak cztery rodziny GEMV rozjechały się na dwudziestu jeden
/// ramionach i Q4_K trafiło w jednej ścieżce na inny kernel niż w drugiej.
/// Należy do wykonawcy, bo to on wie, czym mnoży — a dodanie kwantyzacji jest
/// tu jednym wierszem.
///
/// `match` nie ma gałęzi „reszta": kwantyzacja spoza tabeli ma się TU odbić, a
/// nie policzyć czymkolwiek.
macro_rules! block_formats {
    (
        plain { $($k:ident => $gemv:ident, $gemm:ident, $out_f32:ident;)+ }
        scaled { $($sk:ident => $sgemv:ident, $sgemm:ident, $sout:ident;)+ }
    ) => {
        impl CudaExec {
            /// Czy ten wykonawca ma kernele dla tej kwantyzacji.
            pub(super) fn knows(quant: QuantKind) -> bool {
                matches!(
                    quant,
                    QuantKind::Q4K | QuantKind::Q6K
                        | $(QuantKind::$k)|+
                        | $(QuantKind::$sk)|+
                )
            }

            /// Czy kernele tego formatu biorą skalar całego tensora.
            pub(super) fn scaled(quant: QuantKind) -> bool {
                matches!(quant, $(QuantKind::$sk)|+)
            }

            #[allow(clippy::too_many_arguments)]
            pub(super) fn gemv_by_kind(
                &self,
                quant: QuantKind,
                y: &DevBuffer,
                w: &DevBuffer,
                x: &DevBuffer,
                rows: usize,
                cols: usize,
                scale: f32,
            ) -> Result<()> {
                match quant {
                    QuantKind::Q4K => {
                        self.kernels.gemv_q4_k_f16(y, w, x, rows, cols, &self.stream)
                    }
                    QuantKind::Q6K => {
                        self.kernels.gemv_q6_k_f16(y, w, x, rows, cols, &self.stream)
                    }
                    $(QuantKind::$k => self.kernels.$gemv(y, w, x, rows, cols, &self.stream),)+
                    $(QuantKind::$sk => {
                        self.kernels.$sgemv(y, w, x, rows, cols, scale, &self.stream)
                    })+
                    other => Err(ForgeError::Unsupported(format!("{other:?}: brak GEMV"))),
                }
            }

            #[allow(clippy::too_many_arguments)]
            /// `w_off` addresses ONE expert inside a flat stack: every kernel
            /// of the plain section takes a byte offset into the weights, so a
            /// grouped mixture needs no separate family. The scaled section has
            /// no such parameter, so a stack in one of those formats stops here
            /// rather than multiplying the wrong expert's rows.
            pub(super) fn gemm_by_kind(
                &self,
                quant: QuantKind,
                y: &DevBuffer,
                w: &DevBuffer,
                w_off: usize,
                x: &DevBuffer,
                rows: usize,
                cols: usize,
                n_tokens: usize,
                scale: f32,
            ) -> Result<()> {
                match quant {
                    // The INTEGER MATRIX-UNIT form, not the portable f16 tile.
                    // Both compute the same product; the difference is whether
                    // the card's matrix units run at all, and a prompt is where
                    // that is the whole cost. The launcher picks among its own
                    // variants by shape and token count and always covers.
                    QuantKind::Q4K => self
                        .kernels
                        .gemm_q4_k_i8mma_at(y, w, w_off, x, rows, cols, n_tokens, &self.stream),
                    QuantKind::Q6K => {
                        self.kernels.gemm_q6_k_f16_at(y, w, w_off, x, rows, cols, n_tokens, &self.stream)
                    }
                    $(QuantKind::$k => {
                        self.kernels.$gemm(y, w, w_off, x, rows, cols, n_tokens, &self.stream)
                    })+
                    $(QuantKind::$sk => {
                        if w_off != 0 {
                            return Err(ForgeError::Unsupported(format!(
                                "{quant:?}: GEMM tego formatu nie adresuje wagi przesunięciem"
                            )));
                        }
                        self.kernels.$sgemm(y, w, x, rows, cols, n_tokens, scale, &self.stream)
                    })+
                    other => Err(ForgeError::Unsupported(format!("{other:?}: brak GEMM"))),
                }
            }

            #[allow(clippy::too_many_arguments)]
            pub(super) fn gemv_out_f32_by_kind(
                &self,
                quant: QuantKind,
                y: &DevBuffer,
                w: &DevBuffer,
                x: &DevBuffer,
                rows: usize,
                cols: usize,
                scale: f32,
            ) -> Result<()> {
                match quant {
                    $(QuantKind::$k => {
                        self.kernels.$out_f32(y, w, x, rows, cols, &self.stream)
                    })+
                    $(QuantKind::$sk => {
                        self.kernels.$sout(y, w, x, rows, cols, scale, &self.stream)
                    })+
                    other => Err(ForgeError::Unsupported(format!("{other:?}: brak głowy f32"))),
                }
            }
        }
    };
}

block_formats! {
    plain {
    None   => gemv_f16        , gemm_f16_at        , gemv_f16_out_f32;
    Q5K    => gemv_q5_k_f16   , gemm_q5_k_f16_at   , gemv_q5_k_out_f32;
    Q8_0   => gemv_q8_0_f16   , gemm_q8_0_i8mma_at , gemv_q8_0_out_f32;
    Q3K    => gemv_q3_k_f16   , gemm_q3_k_f16_at   , gemv_q3_k_out_f32;
    Q2K    => gemv_q2_k_f16   , gemm_q2_k_f16_at   , gemv_q2_k_out_f32;
    Q4_0   => gemv_q4_0_f16   , gemm_q4_0_f16_at   , gemv_q4_0_out_f32;
    Q4_1   => gemv_q4_1_f16   , gemm_q4_1_f16_at   , gemv_q4_1_out_f32;
    Q5_0   => gemv_q5_0_f16   , gemm_q5_0_f16_at   , gemv_q5_0_out_f32;
    Q5_1   => gemv_q5_1_f16   , gemm_q5_1_f16_at   , gemv_q5_1_out_f32;
    IQ4NL  => gemv_iq4_nl_f16 , gemm_iq4_nl_f16_at , gemv_iq4_nl_out_f32;
    IQ4XS  => gemv_iq4_xs_f16 , gemm_iq4_xs_f16_at , gemv_iq4_xs_out_f32;
    MXFP4  => gemv_mxfp4_f16  , gemm_mxfp4_f16_at  , gemv_mxfp4_out_f32;
    IQ2XS  => gemv_iq2_xs_f16 , gemm_iq2_xs_f16_at , gemv_iq2_xs_out_f32;
    IQ2S   => gemv_iq2_s_f16  , gemm_iq2_s_f16_at  , gemv_iq2_s_out_f32;
    IQ3S   => gemv_iq3_s_f16  , gemm_iq3_s_f16_at  , gemv_iq3_s_out_f32;
    IQ2XXS => gemv_iq2_xxs_f16, gemm_iq2_xxs_f16_at, gemv_iq2_xxs_out_f32;
    IQ3XXS => gemv_iq3_xxs_f16, gemm_iq3_xxs_f16_at, gemv_iq3_xxs_out_f32;
    IQ1S   => gemv_iq1_s_f16  , gemm_iq1_s_f16_at  , gemv_iq1_s_out_f32;
    IQ1M   => gemv_iq1_m_f16  , gemm_iq1_m_f16_at  , gemv_iq1_m_out_f32;
    }
    // Kernele NVFP4 biorą skalar całego tensora, bo w bloku siedzi tylko
    // czterobitowy wykładnik na szesnaście wartości — reszta zakresu jest w tym
    // jednym mnożniku. To różnica w TREŚCI wywołania, nie w nazwie kernela, i
    // dlatego osobna sekcja, a nie kolejny wiersz.
    scaled {
    NVFP4Gguf => gemv_nvfp4_gguf_f16, gemm_nvfp4_gguf_f16, gemv_nvfp4_gguf_out_f32;
    }
}

impl CudaExec {
    /// Jeden wiersz przez wagę: forma z aktywacją w int8, gdy format ją ma i
    /// szerokość się mieści, inaczej tablica formatów.
    ///
    /// `rows` jest osobnym argumentem, bo warstwa rekurencyjna liczy oknem
    /// węższym niż cała waga.
    pub(super) fn gemv_decode(
        &self,
        w: &Quantized,
        y: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
    ) -> Result<()> {
        if w.cols <= Kernels::DP4A_MAX_COLS {
            let s = &self.stream;
            match w.quant {
                QuantKind::Q8_0 => {
                    return self.kernels.gemv_q8_0_dp4a_f16(y, &w.blocks, x, rows, w.cols, s);
                }
                QuantKind::Q6K => {
                    return self.kernels.gemv_q6_k_dp4a_f16(y, &w.blocks, x, rows, w.cols, s);
                }
                QuantKind::Q4K => {
                    return self.kernels.gemv_q4_k_dp4a_f16(y, &w.blocks, x, rows, w.cols, s);
                }
                _ => {}
            }
        }
        self.gemv_by_kind(w.quant, y, &w.blocks, x, rows, w.cols, w.output_scale)
    }
}
