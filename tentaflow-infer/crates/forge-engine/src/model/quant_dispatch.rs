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
            $($v:ident => $norm:ident, $silu:ident, $gemv:ident, $resid:ident,
                          $rows:ident, $outf32:ident;)+
        }
        dp4a {
            $($dv:ident => $dnorm:ident, $dsilu:ident,
                           $dgemv:ident, $dgemv_wide:ident,
                           $dresid:ident, $dresid_wide:ident;)+
        }
        q4k {
            $q4:ident => $q4norm:ident, $q4silu:ident,
                         $q4gemv:ident, $q4gemv_wide:ident,
                         $q4resid:ident, $q4resid_wide:ident;
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
                    DevWeight::$q4 { buf, rows, cols } => {
                        if *cols <= Kernels::DP4A_MAX_COLS {
                            self.kernels.$q4gemv(
                                y,
                                buf,
                                x,
                                *rows,
                                *cols,
                                self.q4k_decode_model_family(),
                                stream,
                            )
                        } else {
                            self.kernels.$q4gemv_wide(y, buf, x, *rows, *cols, stream)
                        }
                    }
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
                    DevWeight::$q4 { buf, rows, cols } => {
                        if *cols <= Kernels::DP4A_MAX_COLS {
                            self.kernels.$q4resid(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                        } else {
                            self.kernels.$q4resid_wide(&b.h, &b.h32, buf, x, *rows, *cols, stream)
                        }
                    }
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
                    DevWeight::$q4 { buf, rows, cols } => self.kernels.$q4norm(
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
                    DevWeight::$q4 { buf, rows, cols } => self.kernels.$q4silu(
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
        /// Batched GEMM over a row window of `w`: y = W[row_off..row_off+n_rows]·x.
        /// Row offsets translate to per-format byte offsets into the weight (and,
        /// for NVFP4, scale) streams — this is how prefill reads the q/k/v and
        /// gate/up sections out of a fused matrix without storing them twice.
        #[allow(clippy::too_many_arguments)]
        pub(crate) fn gemm_rows(
            &self,
            y: &DevBuffer,
            w: &DevWeight,
            x: &DevBuffer,
            n_tokens: usize,
            row_off: usize,
            n_rows: usize,
            stream: &Stream,
        ) -> Result<()> {
            // Przesunięcie wiersza podaje format z własnej geometrii bloku.
            // Miejsce wywołania przepisywało je wcześniej osobno dla każdego
            // z osiemnastu formatów, a rozmiar bloku i długość bloku w bajtach
            // są tą samą wiedzą, którą `QuantKind` już niesie.
            let row_bytes = || -> Result<usize> {
                w.row_offset_bytes(row_off).ok_or_else(|| {
                    ForgeError::Unsupported(
                        "ten format nie adresuje wiersza jednym przesunięciem".into(),
                    )
                })
            };
            match w {
                    $(
                    DevWeight::$v { buf, cols, .. } => self.kernels.$rows(
                        y,
                        buf,
                        row_bytes()?,
                        x,
                        n_rows,
                        *cols,
                        n_tokens,
                        stream,
                    ),
                    // Q8_0 / Q4_K prefill run the int8 TENSOR-CORE MMQ GEMM: activations
                    // quantized to q8_1, weights kept as native codes, s8xs8->s32 mma
                    // (m16n8k32) per 32-block, then per-block scale/min to f16. This is
                    // the only path that beats the f16 tensor-core GEMM on Ada (2x MAC
                    // throughput + zero dequant bandwidth). Decode still uses the dp4a
                    // GEMV (see gemv). Marshalling the mma's 4x s32 output uses
                    // inlined_assembly + _RegisterPackType (see kernels/mojo/MOJO_NOTES.md).
                    )+
                // Wagi FP8 ze skalą wierszową mają na razie tylko prostą ścieżkę
                // GEMV; pozostałe warianty powstaną razem z mikserem DeepSeeka.
                DevWeight::Fp8Row { .. } => Err(ForgeError::Unsupported(
                    "wagi FP8 ze skalą wierszową nie mają tej ścieżki".into(),
                )),
                DevWeight::Q8_0 { buf, cols, .. } => {
                    let off = row_bytes()?;
                    // Jeden token bierze ten sam dp4a GEMV co dekod jednosekwencyjny.
                    // Kafel i8mma dopełnia do >=64 tokenów i kwantyzuje aktywacje
                    // inaczej, więc ścieżka batchowa dla B=1 dawała trwale inne
                    // logity niż serialna przy zerowym zysku wydajności.
                    if n_tokens == 1 {
                        return self
                            .kernels
                            .gemv_q8_0_dp4a_f16_at(y, buf, off, x, n_rows, *cols, stream);
                    }
                    if self
                        .kernels
                        .gemm_q8_0_small_batch_at(y, buf, off, x, n_rows, *cols, n_tokens, stream)?
                    {
                        return Ok(());
                    }
                    self.kernels
                        .gemm_q8_0_i8mma_at(y, buf, off, x, n_rows, *cols, n_tokens, stream)
                }
                // Small decode batches (T=2/4/8/16) take the weight-stationary
                // dp4a GEMV: one weight sweep serves every token instead of the
                // >=64-token tile the GEMM kernels pad to.
                DevWeight::Q4K { buf, cols, .. } => {
                    let off = row_bytes()?;
                    // Ten sam wniosek co dla Q8_0 wyżej, i ta sama liczba: kafel
                    // i8mma dopełnia do >=64 tokenów, więc przy jednym liczy
                    // sześćdziesiąt trzy puste wiersze. W profilu dekodowania
                    // Qwen3-30B stał za 28% kroku.
                    if n_tokens == 1 {
                        return self
                            .kernels
                            .gemv_q4_k_dp4a_f16_at(y, buf, off, x, n_rows, *cols, stream);
                    }
                    if self
                        .kernels
                        .gemm_qk_dp4a_batch_at(y, buf, off, x, n_rows, *cols, n_tokens, false, stream)?
                    {
                        return Ok(());
                    }
                    self.kernels
                        .gemm_q4_k_i8mma_at(y, buf, off, x, n_rows, *cols, n_tokens, stream)
                }
                DevWeight::Q6K { buf, cols, .. } => {
                    let off = row_bytes()?;
                    if self
                        .kernels
                        .gemm_qk_dp4a_batch_at(y, buf, off, x, n_rows, *cols, n_tokens, true, stream)?
                    {
                        return Ok(());
                    }
                    self.kernels
                        .gemm_q6_k_f16_at(y, buf, off, x, n_rows, *cols, n_tokens, stream)
                }
                DevWeight::NvFp4 {
                    storage,
                    inv_global_scale,
                    cols,
                    ..
                } => match storage {
                    NvFp4CtStorage::RowMajorE4M3 { packed, scales } => self.kernels.gemm_nvfp4_f16_at(
                        y,
                        packed,
                        row_off * (cols / 2),
                        scales,
                        row_off * (cols / 16),
                        x,
                        n_rows,
                        *cols,
                        n_tokens,
                        *inv_global_scale,
                        stream,
                    ),
                    NvFp4CtStorage::S0N64K128 { .. } => {
                        let window = w.nvfp4_ct_row_window(row_off, n_rows)?;
                        let view =
                            Nvfp4CtS0View::new(window.data(), window.physical_rows(), window.cols())?;
                        if n_tokens <= 16 {
                            return self.kernels.gemv_batch_nvfp4_ct_s0_n64k128_f16_at(
                                y,
                                0,
                                view,
                                x,
                                0,
                                window.row_offset(),
                                window.rows(),
                                n_tokens,
                                *inv_global_scale,
                                stream,
                            );
                        }
                        self.kernels.gemm_nvfp4_ct_s0_f16_at(
                            y,
                            view,
                            x,
                            window.row_offset(),
                            window.rows(),
                            n_tokens,
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
                } if row_off == 0 && n_rows == *rows => self.kernels.gemm_nvfp4_gguf_layout_f16(
                    y,
                    buf,
                    x,
                    *rows,
                    *cols,
                    n_tokens,
                    *output_scale,
                    *layout,
                    stream,
                ),
                DevWeight::NvFp4Gguf { .. } => Err(ForgeError::Unsupported(
                    "GGUF NVFP4 GEMM nie obsługuje okna wierszy".into(),
                )),
            }
        }

        /// Projekcja z wyjściem f32, bez obróbki właściwej głowie logitów.
        ///
        /// Ma dwóch wołających i oba potrzebują dokładnie tego samego: głowa logitów
        /// (która dokłada cap i maskę) oraz macierz WIERSZOWO równoległa podziału na
        /// rangi, której wynik jest sumą CZĄSTKOWĄ. Kontrakt liczbowy podziału
        /// wymaga, żeby ranga akumulowała w f32 i żeby zawężenie do f16 nastąpiło
        /// dopiero PO sumie — czyli dokładnie tego, co daje ta rodzina kerneli.
        pub(crate) fn gemv_out_f32(
            &self,
            y_f32: &DevBuffer,
            y_off: usize,
            x: &DevBuffer,
            x_off: usize,
            weight: &DevWeight,
            stream: &Stream,
        ) -> Result<()> {
            if (y_off != 0 || x_off != 0)
                && !matches!(weight, DevWeight::Q4K { .. } | DevWeight::Q6K { .. })
            {
                return Err(ForgeError::Unsupported(
                    "gemv z wyjściem f32 i offsetem lane obsługuje tylko Q4_K/Q6_K".into(),
                ));
            }
            let out = match weight {
                    $(
                    DevWeight::$v { buf, rows, cols } => self
                        .kernels
                        .$outf32(y_f32, buf, x, *rows, *cols, stream),
                    )+
                // Wagi FP8 ze skalą wierszową mają wariant GEMV z wyjściem f16;
                // głowa logitów potrzebuje f32, więc dostanie własną ścieżkę razem
                // z mikserem DeepSeeka.
                DevWeight::Fp8Row { .. } => {
                    return Err(ForgeError::Unsupported(
                        "wagi FP8 ze skalą wierszową nie mają GEMV z wyjściem f32".into(),
                    ))
                }
                DevWeight::Q8_0 { buf, rows, cols } => self
                    .kernels
                    .gemv_q8_0_out_f32(y_f32, buf, x, *rows, *cols, stream),
                DevWeight::Q4K { buf, rows, cols } => {
                    if *cols <= Kernels::DP4A_MAX_COLS {
                        self.kernels
                            .gemv_q4_k_dp4a_out_f32(y_f32, y_off, buf, x, x_off, *rows, *cols, stream)
                    } else {
                        self.kernels
                            .gemv_q4_k_out_f32(y_f32, y_off, buf, x, x_off, *rows, *cols, stream)
                    }
                }
                DevWeight::Q6K { buf, rows, cols } => {
                    if *cols <= Kernels::DP4A_MAX_COLS {
                        self.kernels
                            .gemv_q6_k_dp4a_out_f32(y_f32, y_off, buf, x, x_off, *rows, *cols, stream)
                    } else {
                        self.kernels
                            .gemv_q6_k_out_f32(y_f32, y_off, buf, x, x_off, *rows, *cols, stream)
                    }
                }
                DevWeight::NvFp4 { .. } => Err(ForgeError::Unsupported(
                    "NVFP4 compressed-tensors nie ma kernela GEMV z wyjściem f32".into(),
                )),
                DevWeight::NvFp4Gguf {
                    buf,
                    output_scale,
                    rows,
                    cols,
                    layout,
                } => self.kernels.gemv_nvfp4_gguf_out_f32(
                    if *layout == Nvfp4GgufLayout::RowMajor36 {
                        y_f32
                    } else {
                        return Err(ForgeError::Unsupported(
                            "GEMV z wyjściem f32 nie obsługuje TileN128K64".into(),
                        ));
                    },
                    buf,
                    x,
                    *rows,
                    *cols,
                    *output_scale,
                    stream,
                ),
            };
            out
        }
        }
    };
}

// Format => kernel dla `gemv_norm`, `gemv_norm_silu`, `gemv`, `gemv_residual`.
block_quant_gemv_families! {
    plain {
        F16    => gemv_norm_f16        , gemv_norm_silu_f16        , gemv_f16        , gemv_residual_f16,
                  gemm_f16_at, gemv_f16_out_f32;
        Q5K    => gemv_norm_q5_k_f16   , gemv_norm_silu_q5_k_f16   , gemv_q5_k_f16   , gemv_residual_q5_k_f16,
                  gemm_q5_k_f16_at, gemv_q5_k_out_f32;
        Q3K    => gemv_norm_q3_k_f16   , gemv_norm_silu_q3_k_f16   , gemv_q3_k_f16   , gemv_residual_q3_k_f16,
                  gemm_q3_k_f16_at, gemv_q3_k_out_f32;
        Q2K    => gemv_norm_q2_k_f16   , gemv_norm_silu_q2_k_f16   , gemv_q2_k_f16   , gemv_residual_q2_k_f16,
                  gemm_q2_k_f16_at, gemv_q2_k_out_f32;
        Q4_0   => gemv_norm_q4_0_f16   , gemv_norm_silu_q4_0_f16   , gemv_q4_0_f16   , gemv_residual_q4_0_f16,
                  gemm_q4_0_f16_at, gemv_q4_0_out_f32;
        Q4_1   => gemv_norm_q4_1_f16   , gemv_norm_silu_q4_1_f16   , gemv_q4_1_f16   , gemv_residual_q4_1_f16,
                  gemm_q4_1_f16_at, gemv_q4_1_out_f32;
        Q5_0   => gemv_norm_q5_0_f16   , gemv_norm_silu_q5_0_f16   , gemv_q5_0_f16   , gemv_residual_q5_0_f16,
                  gemm_q5_0_f16_at, gemv_q5_0_out_f32;
        Q5_1   => gemv_norm_q5_1_f16   , gemv_norm_silu_q5_1_f16   , gemv_q5_1_f16   , gemv_residual_q5_1_f16,
                  gemm_q5_1_f16_at, gemv_q5_1_out_f32;
        Iq4Nl  => gemv_norm_iq4_nl_f16 , gemv_norm_silu_iq4_nl_f16 , gemv_iq4_nl_f16 , gemv_residual_iq4_nl_f16,
                  gemm_iq4_nl_f16_at, gemv_iq4_nl_out_f32;
        Iq4Xs  => gemv_norm_iq4_xs_f16 , gemv_norm_silu_iq4_xs_f16 , gemv_iq4_xs_f16 , gemv_residual_iq4_xs_f16,
                  gemm_iq4_xs_f16_at, gemv_iq4_xs_out_f32;
        Mxfp4  => gemv_norm_mxfp4_f16  , gemv_norm_silu_mxfp4_f16  , gemv_mxfp4_f16  , gemv_residual_mxfp4_f16,
                  gemm_mxfp4_f16_at, gemv_mxfp4_out_f32;
        Iq2Xs  => gemv_norm_iq2_xs_f16 , gemv_norm_silu_iq2_xs_f16 , gemv_iq2_xs_f16 , gemv_residual_iq2_xs_f16,
                  gemm_iq2_xs_f16_at, gemv_iq2_xs_out_f32;
        Iq2S   => gemv_norm_iq2_s_f16  , gemv_norm_silu_iq2_s_f16  , gemv_iq2_s_f16  , gemv_residual_iq2_s_f16,
                  gemm_iq2_s_f16_at, gemv_iq2_s_out_f32;
        Iq3S   => gemv_norm_iq3_s_f16  , gemv_norm_silu_iq3_s_f16  , gemv_iq3_s_f16  , gemv_residual_iq3_s_f16,
                  gemm_iq3_s_f16_at, gemv_iq3_s_out_f32;
        Iq2Xxs => gemv_norm_iq2_xxs_f16, gemv_norm_silu_iq2_xxs_f16, gemv_iq2_xxs_f16, gemv_residual_iq2_xxs_f16,
                  gemm_iq2_xxs_f16_at, gemv_iq2_xxs_out_f32;
        Iq3Xxs => gemv_norm_iq3_xxs_f16, gemv_norm_silu_iq3_xxs_f16, gemv_iq3_xxs_f16, gemv_residual_iq3_xxs_f16,
                  gemm_iq3_xxs_f16_at, gemv_iq3_xxs_out_f32;
        Iq1S   => gemv_norm_iq1_s_f16  , gemv_norm_silu_iq1_s_f16  , gemv_iq1_s_f16  , gemv_residual_iq1_s_f16,
                  gemm_iq1_s_f16_at, gemv_iq1_s_out_f32;
        Iq1M   => gemv_norm_iq1_m_f16  , gemv_norm_silu_iq1_m_f16  , gemv_iq1_m_f16  , gemv_residual_iq1_m_f16,
                  gemm_iq1_m_f16_at, gemv_iq1_m_out_f32;
    }
    // norma, norma+silu, następnie para (dp4a, szerokie kolumny) dla `gemv`
    // i dla `gemv_residual`.
    dp4a {
        Q8_0 => gemv_norm_q8_0_dp4a_f16, gemv_norm_silu_q8_0_dp4a_f16,
                gemv_q8_0_dp4a_f16, gemv_q8_0_f16,
                gemv_residual_q8_0_dp4a_f16, gemv_residual_q8_0_f16;
        Q6K  => gemv_norm_q6_k_dp4a_f16, gemv_norm_silu_q6_k_dp4a_f16,
                gemv_q6_k_dp4a_f16, gemv_q6_k_f16,
                gemv_residual_q6_k_dp4a_f16, gemv_residual_q6_k_f16;
    }
    q4k {
        Q4K => gemv_norm_q4_k_dp4a_f16, gemv_norm_silu_q4_k_dp4a_f16,
               gemv_q4_k_dp4a_f16, gemv_q4_k_f16,
               gemv_residual_q4_k_dp4a_f16, gemv_residual_q4_k_f16;
    }
}
