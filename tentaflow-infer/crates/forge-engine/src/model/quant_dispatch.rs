// ===== File: model/quant_dispatch.rs — jedna tabela formatów dla ścieżek, w których format wybiera tylko kernel =====
//
// `gemv_norm` i `gemv_norm_silu` miały po 21 ramion dla formatów blokowych, a
// wyciąg z kodu pokazał, że WSZYSTKIE 42 dzielą dokładnie jedną listę
// argumentów: format wpływał wyłącznie na nazwę kernela. Były więc dwiema
// kopiami tej samej tabeli, rozjeżdżającymi się przy każdej nowej kwantyzacji
// — i dokładnie tak powstała luka, przez którą Q4_K trafiło do jednej ścieżki,
// a do drugiej nie.
//
// Teraz tabela jest jedna, a dodanie kwantyzacji do obu ścieżek to jeden
// wiersz. Formaty, które różnią się TREŚCIĄ, a nie tylko nazwą (`Fp8Row` bez
// tej ścieżki, `NvFp4` z wyborem po układzie pamięci, `NvFp4Gguf` jeszcze
// nieobsługiwany), zostają wypisane osobno w ciele makra — bo one naprawdę są
// osobnymi przypadkami.
//
// `match` pozostaje wyczerpujący i bez gałęzi `_ =>`: nowy wariant
// `DevWeight` ma tu nie skompilować się, dopóki ktoś nie powie, co z nim
// zrobić.

use super::*;

macro_rules! block_quant_gemv_norm_families {
    ($($variant:ident => $norm:ident, $silu:ident;)+) => {
        impl Model {
            /// Fused rmsnorm-recompute + GEMV over the decode residual pair (h, h32).
            pub(crate) fn gemv_norm(
                &self,
                y: &DevBuffer,
                w: &DevWeight,
                norm_w: &DevBuffer,
                ss_from_h16: bool,
                eps: f32,
                stream: &Stream,
            ) -> Result<()> {
                let b = &self.bufs;
                match w {
                    $(
                        DevWeight::$variant { buf, rows, cols } => self.kernels.$norm(
                            y,
                            buf,
                            &b.h,
                            &b.h32,
                            norm_w,
                            *rows,
                            *cols,
                            ss_from_h16,
                            eps,
                            stream,
                        ),
                    )+
                    // Wagi FP8 ze skalą wierszową mają na razie tylko prostą ścieżkę
                    // GEMV; pozostałe warianty powstaną razem z mikserem DeepSeeka.
                    DevWeight::Fp8Row { .. } => Err(ForgeError::Unsupported(
                        "wagi FP8 ze skalą wierszową nie mają tej ścieżki".into(),
                    )),
                    DevWeight::NvFp4 {
                        storage,
                        inv_global_scale,
                        rows,
                        cols,
                    } => match storage {
                        NvFp4CtStorage::RowMajorE4M3 { packed, scales } => {
                            self.kernels.gemv_norm_nvfp4_f16(
                                y,
                                packed,
                                scales,
                                &b.h,
                                &b.h32,
                                norm_w,
                                *rows,
                                *cols,
                                *inv_global_scale,
                                ss_from_h16,
                                eps,
                                stream,
                            )
                        }
                        NvFp4CtStorage::S0N64K128 { data } => {
                            let view = Nvfp4CtS0View::new(data, *rows, *cols)?;
                            self.kernels.gemv_norm_nvfp4_ct_s0_f16(
                                y,
                                view,
                                &b.h,
                                &b.h32,
                                norm_w,
                                *inv_global_scale,
                                ss_from_h16,
                                eps,
                                stream,
                            )
                        }
                    },
                    DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                        "scalony gemv_norm nie obsługuje jeszcze GGUF NVFP4".into(),
                    )),
                }
            }

            /// Fused rmsnorm-recompute + gate|up GEMV + SiLU. `w` is the fused
            /// gate|up matrix; its row count is 2 * inter.
            pub(crate) fn gemv_norm_silu(
                &self,
                act: &DevBuffer,
                w: &DevWeight,
                norm_w: &DevBuffer,
                eps: f32,
                stream: &Stream,
            ) -> Result<()> {
                let b = &self.bufs;
                match w {
                    $(
                        DevWeight::$variant { buf, rows, cols } => self.kernels.$silu(
                            act,
                            buf,
                            &b.h,
                            &b.h32,
                            norm_w,
                            rows / 2,
                            *cols,
                            eps,
                            stream,
                        ),
                    )+
                    DevWeight::Fp8Row { .. } => Err(ForgeError::Unsupported(
                        "wagi FP8 ze skalą wierszową nie mają tej ścieżki".into(),
                    )),
                    DevWeight::NvFp4 {
                        storage,
                        inv_global_scale,
                        rows,
                        cols,
                    } => match storage {
                        NvFp4CtStorage::RowMajorE4M3 { packed, scales } => {
                            self.kernels.gemv_norm_silu_nvfp4_f16(
                                act,
                                packed,
                                scales,
                                &b.h,
                                &b.h32,
                                norm_w,
                                rows / 2,
                                *cols,
                                *inv_global_scale,
                                eps,
                                stream,
                            )
                        }
                        NvFp4CtStorage::S0N64K128 { data } => {
                            let view = Nvfp4CtS0View::new(data, *rows, *cols)?;
                            self.kernels.gemv_norm_silu_nvfp4_ct_s0_f16(
                                act,
                                view,
                                &b.h,
                                &b.h32,
                                norm_w,
                                rows / 2,
                                *inv_global_scale,
                                eps,
                                stream,
                            )
                        }
                    },
                    DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                        "scalony gemv_norm_silu nie obsługuje jeszcze GGUF NVFP4".into(),
                    )),
                }
            }
        }
    };
}

// Format => kernel dla `gemv_norm`, kernel dla `gemv_norm_silu`.
// Q4K, Q6K i Q8_0 idą wariantami DP4A — to jedyny wyłom w konwencji nazw.
block_quant_gemv_norm_families! {
    F16    => gemv_norm_f16          , gemv_norm_silu_f16;
    Q8_0   => gemv_norm_q8_0_dp4a_f16, gemv_norm_silu_q8_0_dp4a_f16;
    Q4K    => gemv_norm_q4_k_dp4a_f16, gemv_norm_silu_q4_k_dp4a_f16;
    Q6K    => gemv_norm_q6_k_dp4a_f16, gemv_norm_silu_q6_k_dp4a_f16;
    Q5K    => gemv_norm_q5_k_f16     , gemv_norm_silu_q5_k_f16;
    Q3K    => gemv_norm_q3_k_f16     , gemv_norm_silu_q3_k_f16;
    Q2K    => gemv_norm_q2_k_f16     , gemv_norm_silu_q2_k_f16;
    Q4_0   => gemv_norm_q4_0_f16     , gemv_norm_silu_q4_0_f16;
    Q4_1   => gemv_norm_q4_1_f16     , gemv_norm_silu_q4_1_f16;
    Q5_0   => gemv_norm_q5_0_f16     , gemv_norm_silu_q5_0_f16;
    Q5_1   => gemv_norm_q5_1_f16     , gemv_norm_silu_q5_1_f16;
    Iq4Nl  => gemv_norm_iq4_nl_f16   , gemv_norm_silu_iq4_nl_f16;
    Iq4Xs  => gemv_norm_iq4_xs_f16   , gemv_norm_silu_iq4_xs_f16;
    Mxfp4  => gemv_norm_mxfp4_f16    , gemv_norm_silu_mxfp4_f16;
    Iq2Xs  => gemv_norm_iq2_xs_f16   , gemv_norm_silu_iq2_xs_f16;
    Iq2S   => gemv_norm_iq2_s_f16    , gemv_norm_silu_iq2_s_f16;
    Iq3S   => gemv_norm_iq3_s_f16    , gemv_norm_silu_iq3_s_f16;
    Iq2Xxs => gemv_norm_iq2_xxs_f16  , gemv_norm_silu_iq2_xxs_f16;
    Iq3Xxs => gemv_norm_iq3_xxs_f16  , gemv_norm_silu_iq3_xxs_f16;
    Iq1S   => gemv_norm_iq1_s_f16    , gemv_norm_silu_iq1_s_f16;
    Iq1M   => gemv_norm_iq1_m_f16    , gemv_norm_silu_iq1_m_f16;
}
