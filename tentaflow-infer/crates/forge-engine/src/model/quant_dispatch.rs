// ===== File: model/quant_dispatch.rs — jedna tabela formatów dla ścieżek, w których format wybiera tylko kernel =====
//
// Cztery rodziny GEMV (`gemv`, `gemv_residual`, `gemv_norm`, `gemv_norm_silu`)
// miały po ~21 ramion dla formatów blokowych, a wyciąg z kodu pokazał, że w
// obrębie rodziny WSZYSTKIE dzielą jedną listę argumentów: format wpływał
// wyłącznie na nazwę kernela. Były więc czterema kopiami tej samej tabeli,
// rozjeżdżającymi się przy każdej nowej kwantyzacji — i dokładnie tak powstała
// luka, przez którą Q4_K trafiło na kernel DP4A w jednej ścieżce, a w drugiej
// nie.
//
// Teraz tabela jest jedna, a dodanie kwantyzacji do wszystkich czterech ścieżek
// to jeden wiersz.
//
// Dwie sekcje, bo formaty dzielą się na dwie klasy, a nie dlatego, że tak
// wyszło:
//   * `plain` — jeden kernel na rodzinę;
//   * `dp4a`  — Q8_0, Q4_K i Q6_K kwantyzują aktywację blokowo i mają próg
//     `DP4A_MAX_COLS` z realnym wariantem zapasowym na szersze kolumny. Sam
//     próg jest jednorodny, więc w tabeli stoi para nazw, a nie osobne ramię.
//
// Formaty różniące się TREŚCIĄ, a nie tylko nazwą, zostają wypisane osobno w
// ciele makra: `Fp8Row` (własny układ ze skalą wierszową), `NvFp4` (wybór po
// układzie pamięci) i `NvFp4Gguf` (własny wybór kernela po układzie i po
// producencie karty).
//
// `match` nie ma gałęzi `_ =>`: nowy wariant `DevWeight` ma tu nie
// skompilować się, dopóki ktoś nie powie, co z nim zrobić.

use super::*;

macro_rules! block_quant_gemv_families {
    (
        plain {
            $($v:ident => $norm:ident, $silu:ident, $gemv:ident, $resid:ident;)+
        }
        dp4a {
            $($dv:ident => $dnorm:ident, $dsilu:ident,
                           $dgemv:ident, $dgemv_wide:ident,
                           $dresid:ident, $dresid_wide:ident;)+
        }
    ) => {
        impl Model {
            pub(crate) fn gemv(
                &self,
                y: &DevBuffer,
                w: &DevWeight,
                x: &DevBuffer,
                stream: &Stream,
            ) -> Result<()> {
                match w {
                    $(
                        DevWeight::$v { buf, rows, cols } => {
                            self.kernels.$gemv(y, buf, x, *rows, *cols, stream)
                        }
                    )+
                    // Q8_0/Q4_K take the int8-activation dp4a kernels (measured faster at
                    // every decode shape); columns beyond the kernels' shared staging
                    // bound keep the f16-x path. Q6_K przez dp4a tam, gdzie się mieści w
                    // oknie aktywacji: rozdzielony pomiar dał 28,2 -> 28,6 tok/s, a
                    // wcześniejsza regresja pochodziła z podniesienia bufora aktywacji
                    // w LDS, nie z samego dp4a.
                    $(
                        DevWeight::$dv { buf, rows, cols } => {
                            if *cols <= Kernels::DP4A_MAX_COLS {
                                self.kernels.$dgemv(y, buf, x, *rows, *cols, stream)
                            } else {
                                self.kernels.$dgemv_wide(y, buf, x, *rows, *cols, stream)
                            }
                        }
                    )+
                    DevWeight::Fp8Row {
                        buf,
                        scales,
                        rows,
                        cols,
                    } => self
                        .kernels
                        .gemv_fp8_row_f16(y, buf, scales, x, *rows, *cols, stream),
                    DevWeight::NvFp4 {
                        storage,
                        inv_global_scale,
                        rows,
                        cols,
                    } => match storage {
                        NvFp4CtStorage::RowMajorE4M3 { packed, scales } => {
                            self.kernels.gemv_nvfp4_f16(
                                y,
                                packed,
                                scales,
                                x,
                                *rows,
                                *cols,
                                *inv_global_scale,
                                stream,
                            )
                        }
                        NvFp4CtStorage::S0N64K128 { .. } => {
                            let window = w.nvfp4_ct_row_window(0, *rows)?;
                            let view = Nvfp4CtS0View::new(
                                window.data(),
                                window.physical_rows(),
                                window.cols(),
                            )?;
                            self.kernels.gemv_nvfp4_ct_s0_n64k128_f16(
                                y,
                                view,
                                x,
                                window.row_offset(),
                                window.rows(),
                                *inv_global_scale,
                                stream,
                            )
                        }
                    },
                    DevWeight::NvFp4Gguf {
                        buf,
                        output_scale,
                        rows,
                        cols,
                        layout,
                    } => {
                        if *layout == Nvfp4GgufLayout::TileN128K64 {
                            self.kernels.gemv_nvfp4_gguf_q8_1_group_layout_f16(
                                &[Nvfp4GgufQ8Projection {
                                    output: y,
                                    weights: buf,
                                    rows: *rows,
                                    output_scale: *output_scale,
                                }],
                                x,
                                *cols,
                                *layout,
                                stream,
                            )
                        } else if self.device.caps().vendor == Vendor::Nvidia {
                            self.kernels
                                .gemv_nvfp4_gguf_b1_f16(y, buf, x, *rows, *cols, *output_scale, stream)
                        } else {
                            self.kernels.gemv_nvfp4_gguf_q8_1_group_f16(
                                &[Nvfp4GgufQ8Projection {
                                    output: y,
                                    weights: buf,
                                    rows: *rows,
                                    output_scale: *output_scale,
                                }],
                                x,
                                *cols,
                                stream,
                            )
                        }
                    }
                }
            }

            /// GEMV + residual add into the decode residual pair (h, h32).
            pub(crate) fn gemv_residual(
                &self,
                w: &DevWeight,
                x: &DevBuffer,
                stream: &Stream,
            ) -> Result<()> {
                let b = &self.bufs;
                match w {
                    $(
                        DevWeight::$v { buf, rows, cols } => self
                            .kernels
                            .$resid(&b.h, &b.h32, buf, x, *rows, *cols, stream),
                    )+
                    // Ta sama polityka progu co w `gemv`.
                    $(
                        DevWeight::$dv { buf, rows, cols } => {
                            if *cols <= Kernels::DP4A_MAX_COLS {
                                self.kernels
                                    .$dresid(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                            } else {
                                self.kernels
                                    .$dresid_wide(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                            }
                        }
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
                            self.kernels.gemv_residual_nvfp4_f16(
                                &b.h,
                                &b.h32,
                                packed,
                                scales,
                                x,
                                *rows,
                                *cols,
                                *inv_global_scale,
                                stream,
                            )
                        }
                        NvFp4CtStorage::S0N64K128 { data } => {
                            let view = Nvfp4CtS0View::new(data, *rows, *cols)?;
                            self.kernels.gemv_residual_nvfp4_ct_s0_f16(
                                &b.h,
                                &b.h32,
                                view,
                                x,
                                *inv_global_scale,
                                stream,
                            )
                        }
                    },
                    DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                        "scalony gemv_residual nie obsługuje jeszcze GGUF NVFP4".into(),
                    )),
                }
            }

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
                        DevWeight::$v { buf, rows, cols } => self.kernels.$norm(
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
                    // Scalona norma ma tylko wariant DP4A — bez progu, bo kernel
                    // zapasowy dla tej ścieżki nie istnieje.
                    $(
                        DevWeight::$dv { buf, rows, cols } => self.kernels.$dnorm(
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
                        DevWeight::$v { buf, rows, cols } => self.kernels.$silu(
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
                    $(
                        DevWeight::$dv { buf, rows, cols } => self.kernels.$dsilu(
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

// Format => kernel dla `gemv_norm`, `gemv_norm_silu`, `gemv`, `gemv_residual`.
block_quant_gemv_families! {
    plain {
        F16    => gemv_norm_f16        , gemv_norm_silu_f16        , gemv_f16        , gemv_residual_f16;
        Q5K    => gemv_norm_q5_k_f16   , gemv_norm_silu_q5_k_f16   , gemv_q5_k_f16   , gemv_residual_q5_k_f16;
        Q3K    => gemv_norm_q3_k_f16   , gemv_norm_silu_q3_k_f16   , gemv_q3_k_f16   , gemv_residual_q3_k_f16;
        Q2K    => gemv_norm_q2_k_f16   , gemv_norm_silu_q2_k_f16   , gemv_q2_k_f16   , gemv_residual_q2_k_f16;
        Q4_0   => gemv_norm_q4_0_f16   , gemv_norm_silu_q4_0_f16   , gemv_q4_0_f16   , gemv_residual_q4_0_f16;
        Q4_1   => gemv_norm_q4_1_f16   , gemv_norm_silu_q4_1_f16   , gemv_q4_1_f16   , gemv_residual_q4_1_f16;
        Q5_0   => gemv_norm_q5_0_f16   , gemv_norm_silu_q5_0_f16   , gemv_q5_0_f16   , gemv_residual_q5_0_f16;
        Q5_1   => gemv_norm_q5_1_f16   , gemv_norm_silu_q5_1_f16   , gemv_q5_1_f16   , gemv_residual_q5_1_f16;
        Iq4Nl  => gemv_norm_iq4_nl_f16 , gemv_norm_silu_iq4_nl_f16 , gemv_iq4_nl_f16 , gemv_residual_iq4_nl_f16;
        Iq4Xs  => gemv_norm_iq4_xs_f16 , gemv_norm_silu_iq4_xs_f16 , gemv_iq4_xs_f16 , gemv_residual_iq4_xs_f16;
        Mxfp4  => gemv_norm_mxfp4_f16  , gemv_norm_silu_mxfp4_f16  , gemv_mxfp4_f16  , gemv_residual_mxfp4_f16;
        Iq2Xs  => gemv_norm_iq2_xs_f16 , gemv_norm_silu_iq2_xs_f16 , gemv_iq2_xs_f16 , gemv_residual_iq2_xs_f16;
        Iq2S   => gemv_norm_iq2_s_f16  , gemv_norm_silu_iq2_s_f16  , gemv_iq2_s_f16  , gemv_residual_iq2_s_f16;
        Iq3S   => gemv_norm_iq3_s_f16  , gemv_norm_silu_iq3_s_f16  , gemv_iq3_s_f16  , gemv_residual_iq3_s_f16;
        Iq2Xxs => gemv_norm_iq2_xxs_f16, gemv_norm_silu_iq2_xxs_f16, gemv_iq2_xxs_f16, gemv_residual_iq2_xxs_f16;
        Iq3Xxs => gemv_norm_iq3_xxs_f16, gemv_norm_silu_iq3_xxs_f16, gemv_iq3_xxs_f16, gemv_residual_iq3_xxs_f16;
        Iq1S   => gemv_norm_iq1_s_f16  , gemv_norm_silu_iq1_s_f16  , gemv_iq1_s_f16  , gemv_residual_iq1_s_f16;
        Iq1M   => gemv_norm_iq1_m_f16  , gemv_norm_silu_iq1_m_f16  , gemv_iq1_m_f16  , gemv_residual_iq1_m_f16;
    }
    // norma, norma+silu, następnie para (dp4a, szerokie kolumny) dla `gemv`
    // i dla `gemv_residual`.
    dp4a {
        Q8_0 => gemv_norm_q8_0_dp4a_f16, gemv_norm_silu_q8_0_dp4a_f16,
                gemv_q8_0_dp4a_f16, gemv_q8_0_f16,
                gemv_residual_q8_0_dp4a_f16, gemv_residual_q8_0_f16;
        Q4K  => gemv_norm_q4_k_dp4a_f16, gemv_norm_silu_q4_k_dp4a_f16,
                gemv_q4_k_dp4a_f16, gemv_q4_k_f16,
                gemv_residual_q4_k_dp4a_f16, gemv_residual_q4_k_f16;
        Q6K  => gemv_norm_q6_k_dp4a_f16, gemv_norm_silu_q6_k_dp4a_f16,
                gemv_q6_k_dp4a_f16, gemv_q6_k_f16,
                gemv_residual_q6_k_dp4a_f16, gemv_residual_q6_k_f16;
    }
}
