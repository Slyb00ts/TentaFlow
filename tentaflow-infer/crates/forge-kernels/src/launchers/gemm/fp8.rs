// ===== File: gemm/fp8.rs — GEMM/GEMV na FP8 e4m3 i W4A8 =====
use super::*;

/// Drabina szerokosci plastrow kolumnowych, malejaco. Kazda pozycja jest
/// wielokrotnoscia 256, wiec z tych, ktore sa w artefaktach, da sie zlozyc
/// dowolne N podzielne przez 256. Powyzej 4096 kolumn ten GEMM i tak sie
/// zalamuje (142 -> 47 TFLOPS miedzy N=4096 a N=11264), wiec drabina tam sie
/// konczy.
const STRIP_LADDER: [usize; 7] = [4096, 3072, 2048, 1536, 1024, 512, 256];

fn strip_kernel_name(width: usize, ldy: usize, k: usize) -> String {
    format!("gemm_fp8_mod_{width}x{ldy}_{k}")
}

/// Rozklad N na plastry kolumnowe policzony z LICZBY SM, nie z tabeli ksztaltow.
///
/// Siatka plastra to `(szerokosc/256, tokeny/128)`. Chcemy, zeby iloczyn wyszedl
/// mniej wiecej na liczbe SM: wtedy wszystkie wiersze M sa rezydentne naraz i
/// dziela ten sam kafel wag, czyli kazda macierz wag idzie przez pamiec RAZ.
/// Przy 48 SM i pelnym kawalku prefillu (1024 tokenow, 8 wierszy M) daje to
/// 1536 kolumn — dokladnie to, co wczesniej bylo wpisane recznie.
///
/// Bez tego kazda macierz wag idzie przez pamiec kilka razy: przy 16 kaflach
/// kolumnowych rezydentne sa tylko 3 wiersze M, wiec wagi krazily TRZY razy.
/// W silniku to jest cala roznica, bo kazda z 40 warstw ma inne wagi i L2 nie
/// pomaga miedzy wywolaniami — `nsys` na prefillu 2048 pokazal 44,1 GB ruchu
/// wag (197 ms) przy podlodze obliczeniowej 118 ms, a jeden przebieg to 14,7 GB
/// (66 ms). Mikrobench tego NIE pokazywal: tam ta sama macierz wracala do L2 co
/// iteracje i plastry wychodzily gorzej.
///
/// Wybor jest sterowany artefaktami: brak instancji o danej szerokosci po prostu
/// wypada z drabiny, wiec dolozenie modelu to dopisanie kerneli w
/// `gemm_fp8_modular.mojo`, bez dotykania tego pliku.
fn fp8_strip_plan(
    rows: usize,
    cols: usize,
    n_tokens: usize,
    sm_count: usize,
    has: impl Fn(&str) -> bool,
) -> Option<Vec<(usize, String)>> {
    if sm_count == 0 || !rows.is_multiple_of(256) {
        return None;
    }
    let available = |w: usize| w < rows && has(&strip_kernel_name(w, rows, cols));
    let m_tiles = n_tokens.div_ceil(128).max(1);
    let target = (256 * sm_count / m_tiles).max(256);
    // Zaokraglamy W GORE, nie w dol: plaster wezszy od celu nie zapelnia
    // maszyny. Przy 2048 tokenach cel wypada na 768, ktorego nie ma w
    // artefaktach — zejscie do 512 daje 2 kafle kolumnowe x 16 wierszy M, czyli
    // 32 bloki na 48 SM, i kosztuje 641 ms wobec 362 ms (zmierzone, prompt
    // 2048). Zaokraglenie w gore bierze 1024, czyli 64 bloki: druga fala jest
    // niepelna, ale zadna nie jest pusta.
    let width = STRIP_LADDER
        .iter()
        .rev()
        .copied()
        .find(|&w| w >= target && available(w))
        .or_else(|| STRIP_LADDER.iter().copied().find(|&w| available(w)))?;

    let mut plan = Vec::new();
    let mut left = rows;
    while left >= width {
        plan.push((width, strip_kernel_name(width, rows, cols)));
        left -= width;
    }
    for w in STRIP_LADDER {
        if !available(w) {
            continue;
        }
        while left >= w {
            plan.push((w, strip_kernel_name(w, rows, cols)));
            left -= w;
        }
    }
    // Ogon, ktorego drabina nie sklada, oraz plan z jednego plastra (czyli
    // zwykle wywolanie) zostawiamy sciezce bazowej.
    if left != 0 || plan.len() < 2 {
        return None;
    }
    Some(plan)
}

impl Kernels {
    pub fn supports_fp8_modular_shape(&self, rows: usize, cols: usize) -> bool {
        self.artifacts.has(&format!("gemm_fp8_mod_{rows}_{cols}"))
    }

    pub fn supports_fp8_logits(&self) -> bool {
        self.artifacts.has("gemv_fp8_out_f32_v2")
    }

    /// SwiGLU + kwantyzacja aktywacji w jednym przejściu.
    ///
    /// Rozdzielnie kosztowało trzy przejścia przez bufor pośredni [T, n_cols]:
    /// `silu_mul` czytał gate i up i zapisywał wynik, a `quantize_act_fp8`
    /// czytał ten wynik dwa razy (absmax, kodowanie). Kernel fuzowany trzyma
    /// wynik w pamięci współdzielonej, więc z HBM idzie tylko odczyt gate i up
    /// oraz zapis kodów.
    ///
    /// Zwraca `false`, gdy artefaktu nie ma albo `n_cols` nie mieści się w
    /// pamięci współdzielonej — wtedy woła się starą parę kerneli.
    #[allow(clippy::too_many_arguments)]
    pub fn silu_mul_quant_fp8(
        &self,
        gate: &DevBuffer,
        up: &DevBuffer,
        n_cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<bool> {
        let smem = n_cols * 2;
        if !self.artifacts.has("silu_mul_quant_fp8")
            || smem > self.device.caps().max_shared_mem_per_block
        {
            return Ok(false);
        }
        // Ta sama pula co u zsynchronizowanej normy, wiec `gemm_fp8_prequant`
        // czyta wynik bez posrednictwa.
        self.fp8_act_ensure(n_tokens * n_cols, n_tokens)?;
        let sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xs = sc.xs.as_ref().expect("xs allocated");
        let k = self.artifacts.get("silu_mul_quant_fp8")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: smem as u32,
        };
        let args = LaunchArgs::new()
            .buf(xq)
            .buf(xs)
            .buf(gate)
            .buf(up)
            .scalar(n_cols as i64)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)?;
        Ok(true)
    }

    /// `gemv_fp8_row_f16` na wierszu wskazanym przesunięciem bajtowym — ścieżka
    /// DeepSeeka liczy projekcje token po tokenie wewnątrz większych buforów.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_fp8_row_f16_at(
        &self,
        y: &DevBuffer,
        y_off: usize,
        w: &DevBuffer,
        scales: &DevBuffer,
        x: &DevBuffer,
        x_off: usize,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_fp8_row_f16 wymaga cols % 256 == 0, otrzymano {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_fp8_row_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(y, y_off)?
            .buf(w)
            .buf(scales)
            .buf_at(x, x_off)?
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// GEMV na wagach E4M3 z jedną skalą FP32 na wiersz, wynik w f16.
    ///
    /// Wariant f32 (`gemv_fp8_out_f32`) służy głowie logitów; ten karmi kolejne
    /// warstwy, których aktywacje są f16, więc zawężenie dzieje się w kernelu
    /// zamiast osobnym przejściem po całym wyjściu.
    pub fn gemv_fp8_row_f16(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        scales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_fp8_row_f16 wymaga cols % 256 == 0, otrzymano {cols}"
            )));
        }
        let k = self.artifacts.get("gemv_fp8_row_f16_v2")?;
        let cfg = LaunchConfig {
            grid: ((rows as u32).div_ceil(8), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(scales)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Logity FP32 z wag E4M3 oraz jednej skali FP32 na wiersz.
    #[allow(clippy::too_many_arguments)]
    pub fn gemv_fp8_out_f32(
        &self,
        y_f32: &DevBuffer,
        w: &DevBuffer,
        scales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if rows == 0 || !cols.is_multiple_of(256) {
            return Err(ForgeError::Kernel(format!(
                "gemv_fp8_out_f32 wymaga cols % 256 == 0, otrzymano {cols}"
            )));
        }
        let output_bytes = rows.checked_mul(4).ok_or_else(|| {
            ForgeError::Kernel("gemv_fp8_out_f32: przepełnienie rozmiaru wyjścia".into())
        })?;
        let weight_bytes = rows.checked_mul(cols).ok_or_else(|| {
            ForgeError::Kernel("gemv_fp8_out_f32: przepełnienie rozmiaru wag".into())
        })?;
        let input_bytes = cols.checked_mul(2).ok_or_else(|| {
            ForgeError::Kernel("gemv_fp8_out_f32: przepełnienie rozmiaru wejścia".into())
        })?;
        let grid_x = u32::try_from(rows.div_ceil(8))
            .map_err(|_| ForgeError::Kernel("gemv_fp8_out_f32: siatka przekracza u32".into()))?;
        if y_f32.len() < output_bytes
            || w.len() < weight_bytes
            || scales.len() < output_bytes
            || x.len() < input_bytes
        {
            return Err(ForgeError::Kernel(
                "gemv_fp8_out_f32: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let k = self.artifacts.get("gemv_fp8_out_f32_v2")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y_f32)
            .buf(w)
            .buf(scales)
            .buf(x)
            .scalar(cols as i64)
            .scalar(rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// QServe W4A8 CTA config for `M` tokens and `K` cols, mirroring
    /// `gemm_forward_cuda`'s host dispatch. Returns
    /// `(registry_key, CTA_M, CTA_N, CTA_K, num_warps, dynamic_smem_bytes)`.
    fn w4a8_config(m: usize, k: usize) -> (&'static str, u32, u32, u32, u32, u32) {
        if m > 128 {
            ("w4a8_gemm_m128", 128, 64, 64, 4, 41472)
        } else if m == 128 {
            if k <= 4096 {
                ("w4a8_gemm_m64_ksm", 64, 64, 64, 4, 25088)
            } else {
                ("w4a8_gemm_m64_klg", 64, 64, 128, 8, 37248)
            }
        } else {
            ("w4a8_gemm_m32", 32, 64, 128, 4, 24960)
        }
    }

    /// W4A8 (int4-weight x int8-activation) prefill GEMM: `y[t,row] = W·x[t]`.
    /// Non-default (routed only under `FORGE_GEMM=w4a8`). Consumes activations
    /// ALREADY quantized to per-token int8 (`a_i8` + `ascales`); the weight
    /// buffers are QServe-packed (`forge_formats::w4a8`). `rows` (N) must be a
    /// multiple of 64 and `cols` (K) a multiple of 128 (the kernel's group).
    #[allow(clippy::too_many_arguments)]
    pub fn w4a8_gemm(
        &self,
        y: &DevBuffer,
        a_i8: &DevBuffer,
        qweight: &DevBuffer,
        s2_zeros: &DevBuffer,
        s2_scales: &DevBuffer,
        wscales: &DevBuffer,
        ascales: &DevBuffer,
        n_tokens: usize,
        rows: usize,
        cols: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !rows.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "w4a8 requires rows % 64 == 0, got {rows}"
            )));
        }
        if !cols.is_multiple_of(128) {
            return Err(ForgeError::Kernel(format!(
                "w4a8 requires cols % 128 == 0, got {cols}"
            )));
        }
        let (key, cta_m, cta_n, cta_k, warps, smem) = Self::w4a8_config(n_tokens, cols);
        if !cols.is_multiple_of(cta_k as usize) {
            return Err(ForgeError::Kernel(format!(
                "w4a8 config {key} needs cols % {cta_k} == 0, got {cols}"
            )));
        }
        let gk = self.artifacts.get(key)?;
        let num_blocks_n = (rows as u32) / cta_n;
        let num_blocks_m = (n_tokens as u32).div_ceil(cta_m);
        let log_tile = if num_blocks_m >= 6 {
            3
        } else if num_blocks_m >= 3 {
            2
        } else if num_blocks_m >= 2 {
            1
        } else {
            0
        };
        let tile_shift = 1u32 << log_tile;
        let cfg = LaunchConfig {
            grid: (
                num_blocks_n * tile_shift,
                num_blocks_m.div_ceil(tile_shift),
                1,
            ),
            block: (32, warps, 1),
            shared_mem_bytes: smem,
        };
        let args = LaunchArgs::new()
            .buf(a_i8)
            .buf(qweight)
            .buf(s2_zeros)
            .buf(s2_scales)
            .buf(wscales)
            .buf(ascales)
            .buf(y)
            .scalar(n_tokens as i64)
            .scalar(rows as i64)
            .scalar(cols as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }

    /// Per-token int8 activation quant + W4A8 GEMM in one call: quantizes the
    /// f16 activation `x` [n_tokens, cols] to symmetric int8 codes + per-token
    /// f16 scale (QServe layout) into grow-only scratch, then runs the int4-
    /// weight x int8-activation GEMM. `y` is f16 [n_tokens, rows]. `inv_smooth`
    /// is the per-input-channel SmoothQuant reciprocal `1/s` (f16 [cols]);
    /// activations are multiplied by it before the int8 quant, matching the
    /// packed weight's per-column `s` scaling. Pass an all-ones buffer for the
    /// identity (no smoothing). Both launches share `stream` (no explicit sync).
    /// Non-default (FORGE_GEMM=w4a8).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_w4a8(
        &self,
        y: &DevBuffer,
        qweight: &DevBuffer,
        s2_zeros: &DevBuffer,
        s2_scales: &DevBuffer,
        wscales: &DevBuffer,
        inv_smooth: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        let need_codes = n_tokens * cols;
        let mut sc = self.w4a8_act.lock().expect("w4a8 act scratch poisoned");
        if sc.cap_codes < need_codes {
            sc.a_i8 = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_tokens < n_tokens {
            sc.ascales = Some(self.device.alloc(
                n_tokens * 2,
                MemKind::Device,
                Pool::Activations,
            )?);
            sc.cap_tokens = n_tokens;
        }
        let a_i8 = sc.a_i8.as_ref().expect("a_i8 allocated");
        let ascales = sc.ascales.as_ref().expect("ascales allocated");

        let qk = self.artifacts.get("w4a8_quant_act")?;
        let block = (cols as u32).clamp(32, 1024);
        let qcfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let qargs = LaunchArgs::new()
            .buf(x)
            .buf(a_i8)
            .buf(ascales)
            .buf(inv_smooth)
            .scalar(n_tokens as i64)
            .scalar(cols as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;

        self.w4a8_gemm(
            y, a_i8, qweight, s2_zeros, s2_scales, wscales, ascales, n_tokens, rows, cols, stream,
        )
    }

    /// Tile selection for the fp8 GEMM: `(suffix, BM, BN, block_threads)`. The
    /// f32 mma accumulate is exact across tile shapes (bit-identical, like the
    /// integer i8mma), so this is a pure perf gate; mirrors `gemm_i8mma_tile`.
    /// Nazwa kernela i geometria kafla FP8. Rodzina `mma` NVIDII i rodzina WMMA
    /// RDNA4 liczą TĘ SAMĄ matematykę i mają ten sam kontrakt argumentów
    /// (`y, w, wscales, xq, xs, cols, rows, n_tokens`), więc wybór idzie
    /// wyłącznie po obecności artefaktu. Kafle RDNA4 zmierzone w
    /// `bench-amd/bench_gemm_fp8_wmma.mojo`.
    fn gemm_fp8_tile(&self, rows: usize, n_tokens: usize) -> (String, u32, u32, u32) {
        if self.artifacts.has("gemm_fp8_wmma_bm512_bn128") {
            if n_tokens <= 32 {
                return ("gemm_fp8_wmma_bm32".into(), 32, 64, 128);
            }
            if n_tokens >= 512 {
                return ("gemm_fp8_wmma_bm512_bn128".into(), 512, 128, 512);
            }
            return ("gemm_fp8_wmma_bm256_bn128".into(), 256, 128, 256);
        }
        let big_blocks = rows.div_ceil(128) * n_tokens.div_ceil(128);
        if n_tokens >= 1024 && big_blocks >= 256 {
            ("gemm_fp8_f16_big".into(), 128, 128, 512)
        } else if n_tokens >= 256 {
            ("gemm_fp8_f16".into(), 128, 64, 256)
        } else {
            ("gemm_fp8_f16_bm64".into(), 64, 64, 256)
        }
    }

    /// Per-token e4m3 activation quant + fp8 (e4m3-weight × e4m3-activation)
    /// prefill GEMM in one call: quantizes f16 `x` [n_tokens, cols] to e4m3
    /// codes + per-token f32 scale into grow-only scratch, then runs the fp8
    /// tensor-core GEMM. `w` is e4m3 bytes [rows, cols], `wscales` the per-row
    /// f32 scale [rows]. `y` is f16 [n_tokens, rows]. Both launches share
    /// `stream` (no explicit sync). `cols % 32 == 0`. Non-default
    /// (FORGE_GEMM=fp8).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_fp8(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        wscales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "gemm_fp8 requires cols % 32 == 0, got {cols}"
            )));
        }
        let need_codes = n_tokens * cols;
        let mut sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        if sc.cap_codes < need_codes {
            sc.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_tokens < n_tokens {
            sc.xs = Some(
                self.device
                    .alloc(n_tokens * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_tokens = n_tokens;
        }
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xs = sc.xs.as_ref().expect("xs allocated");

        // Per-token activation quant: one block per token, block-wide absmax
        // reduction over K (block <= 1024 to fit the shared reduction array).
        let qk = self.artifacts.get("quantize_act_fp8")?;
        let qcfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let qargs = LaunchArgs::new()
            .buf(xq)
            .buf(xs)
            .buf(x)
            .scalar(cols as i64)
            .scalar(n_tokens as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;

        let (name, bm, bn, threads) = self.gemm_fp8_tile(rows, n_tokens);
        let gk = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(bn),
                (n_tokens as u32).div_ceil(bm),
                1,
            ),
            block: (threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(w)
            .buf(wscales)
            .buf(xq)
            .buf(xs)
            .scalar(cols as i64)
            .scalar(rows as i64)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }

    /// Per-token e4m3 activation quant + Modular's multistage cp.async fp8 GEMM
    /// (one kernel per (rows,cols); docs/CODEGEN_PROOF.md Finding G). Same fp8
    /// weight pack + activation quant as `gemm_fp8`, but the GEMM is the deeply
    /// pipelined `multistage_gemm_kernel` (dynamic-M wrapper) that runs at
    /// 260–313 TFLOPS on Ada — 1.3–1.5× the CUDA MMQ — with the per-token ×
    /// per-row scale + f16 downcast fused into its epilogue (no extra HBM pass).
    /// Grid (ceil(rows/128), ceil(n_tokens/128)); block 128; dynamic smem 65536
    /// (the >48 KB opt-in the HAL sets automatically). Non-default
    /// (`FORGE_GEMM=fp8mod`); errors if no committed PTX matches (rows,cols).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_fp8_modular(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        wscales: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "gemm_fp8_modular requires cols % 64 == 0, got {cols}"
            )));
        }
        let caps = self.device.caps();
        let use_bn256 = fp8_modular_bn256_capable(
            caps.vendor,
            caps.warp_size,
            caps.max_threads_per_block,
            caps.max_shared_mem_per_block,
            rows,
            cols,
            n_tokens,
            |name| self.artifacts.has(name),
        );
        let base_kernel_name = format!("gemm_fp8_mod_{rows}_{cols}");
        let kernel_name = if use_bn256 {
            fp8_modular_bn256_kernel(rows, cols).expect("kształt sprawdzony przez capability")
        } else {
            base_kernel_name.as_str()
        };
        let gk = self.artifacts.get(kernel_name).map_err(|_| {
            ForgeError::Kernel(format!(
                "gemm_fp8_modular: no committed Modular fp8 kernel for \
                     (rows={rows}, cols={cols}); build one in gemm_fp8_modular.mojo"
            ))
        })?;

        let need_codes = n_tokens * cols;
        let mut sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        if sc.cap_codes < need_codes {
            sc.xq = Some(
                self.device
                    .alloc(need_codes, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_codes = need_codes;
        }
        if sc.cap_tokens < n_tokens {
            sc.xs = Some(
                self.device
                    .alloc(n_tokens * 4, MemKind::Device, Pool::Activations)?,
            );
            sc.cap_tokens = n_tokens;
        }
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xs = sc.xs.as_ref().expect("xs allocated");

        // Per-token activation quant → e4m3 codes + f32 scale (shared with the
        // hand fp8 path).
        let qk = self.artifacts.get("quantize_act_fp8")?;
        let qcfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let qargs = LaunchArgs::new()
            .buf(xq)
            .buf(xs)
            .buf(x)
            .scalar(cols as i64)
            .scalar(n_tokens as i64);
        self.device.launch(qk, &qcfg, &qargs, stream)?;

        // multistage GEMM: y = diag(xs)·(xq·wᵀ)·diag(ws), fused epilogue. Params
        // mirror gemm_fp8_mod(y, a=xq, b=w, xs, ws, m=n_tokens).
        let (row_tile, block_threads, shared_mem_bytes) = if use_bn256 {
            (256, 256, FP8_MODULAR_BN256_SMEM as u32)
        } else {
            (128, 128, 65_536)
        };
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(row_tile),
                (n_tokens as u32).div_ceil(128),
                1,
            ),
            block: (block_threads, 1, 1),
            shared_mem_bytes,
        };
        // Podzial szerokiego N na kawalki po <=4096 kolumn, gdy komplet kerneli
        // czastkowych jest w artefaktach. Kazdy kawalek dostaje przesuniete
        // wagi (wiersze B), skale wierszy i kolumne startowa w Y.
        if let Some(plan) = fp8_strip_plan(rows, cols, n_tokens, caps.sm_count as usize, |name| {
            self.artifacts.has(name)
        }) {
            let mut start = 0usize;
            for (strip_rows, name) in plan {
                let ck = self.artifacts.get(&name)?;
                let ccfg = LaunchConfig {
                    grid: (
                        (strip_rows as u32).div_ceil(256),
                        (n_tokens as u32).div_ceil(128),
                        1,
                    ),
                    block: (256, 1, 1),
                    shared_mem_bytes: FP8_MODULAR_BN256_SMEM as u32,
                };
                let cargs = LaunchArgs::new()
                    .buf_at(y, start * 2)?
                    .buf(xq)
                    .buf_at(w, start * cols)?
                    .buf(xs)
                    .buf_at(wscales, start * 4)?
                    .scalar(n_tokens as i64);
                self.device.launch(ck, &ccfg, &cargs, stream)?;
                start += strip_rows;
            }
            return Ok(());
        }

        let args = LaunchArgs::new()
            .buf(y)
            .buf(xq)
            .buf(w)
            .buf(xs)
            .buf(wscales)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }

    /// Modular multistage fp8 GEMM over an EXTERNALLY prequantized activation:
    /// reads the shared fp8 activation scratch (`xq`/`xs`) that the preceding
    /// fused rmsnorm→fp8 emitted — NO per-projection quantize pass. `cols` (the
    /// projection K) must match the fused norm's hidden size that filled the
    /// scratch. Otherwise identical to `gemm_fp8_modular`. (`FORGE_GEMM=fp8mod`).
    #[allow(clippy::too_many_arguments)]
    pub fn gemm_fp8_modular_prequant(
        &self,
        y: &DevBuffer,
        w: &DevBuffer,
        wscales: &DevBuffer,
        rows: usize,
        cols: usize,
        n_tokens: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !cols.is_multiple_of(64) {
            return Err(ForgeError::Kernel(format!(
                "gemm_fp8_modular_prequant requires cols % 64 == 0, got {cols}"
            )));
        }
        let caps = self.device.caps();
        let use_bn256 = fp8_modular_bn256_capable(
            caps.vendor,
            caps.warp_size,
            caps.max_threads_per_block,
            caps.max_shared_mem_per_block,
            rows,
            cols,
            n_tokens,
            |name| self.artifacts.has(name),
        );
        let base_kernel_name = format!("gemm_fp8_mod_{rows}_{cols}");
        let kernel_name = if use_bn256 {
            fp8_modular_bn256_kernel(rows, cols).expect("kształt sprawdzony przez capability")
        } else {
            base_kernel_name.as_str()
        };
        let gk = self.artifacts.get(kernel_name).map_err(|_| {
            ForgeError::Kernel(format!(
                "gemm_fp8_modular_prequant: no committed Modular fp8 kernel for \
                     (rows={rows}, cols={cols}); build one in gemm_fp8_modular.mojo"
            ))
        })?;
        let sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        if sc.cap_codes < n_tokens * cols || sc.cap_tokens < n_tokens {
            return Err(ForgeError::Kernel(
                "gemm_fp8_modular_prequant: shared fp8 activation scratch not sized \
                 by a preceding rmsnorm_fp8_shared"
                    .into(),
            ));
        }
        let xq = sc.xq.as_ref().expect("xq filled by fused norm");
        let xs = sc.xs.as_ref().expect("xs filled by fused norm");
        let (row_tile, block_threads, shared_mem_bytes) = if use_bn256 {
            (256, 256, FP8_MODULAR_BN256_SMEM as u32)
        } else {
            (128, 128, 65_536)
        };
        let cfg = LaunchConfig {
            grid: (
                (rows as u32).div_ceil(row_tile),
                (n_tokens as u32).div_ceil(128),
                1,
            ),
            block: (block_threads, 1, 1),
            shared_mem_bytes,
        };
        // Ten sam podzial szerokiego N co w `gemm_fp8_modular`. Tedy idzie FFN
        // gate/up, czyli dokladnie ksztalt (11264, 4096), na ktorym pojedyncze
        // wywolanie osiaga 47 TFLOPS zamiast 142.
        if let Some(plan) = fp8_strip_plan(rows, cols, n_tokens, caps.sm_count as usize, |name| {
            self.artifacts.has(name)
        }) {
            let mut start = 0usize;
            for (strip_rows, name) in plan {
                let ck = self.artifacts.get(&name)?;
                let ccfg = LaunchConfig {
                    grid: (
                        (strip_rows as u32).div_ceil(256),
                        (n_tokens as u32).div_ceil(128),
                        1,
                    ),
                    block: (256, 1, 1),
                    shared_mem_bytes: FP8_MODULAR_BN256_SMEM as u32,
                };
                let cargs = LaunchArgs::new()
                    .buf_at(y, start * 2)?
                    .buf(xq)
                    .buf_at(w, start * cols)?
                    .buf(xs)
                    .buf_at(wscales, start * 4)?
                    .scalar(n_tokens as i64);
                self.device.launch(ck, &ccfg, &cargs, stream)?;
                start += strip_rows;
            }
            return Ok(());
        }

        let args = LaunchArgs::new()
            .buf(y)
            .buf(xq)
            .buf(w)
            .buf(xs)
            .buf(wscales)
            .scalar(n_tokens as i64);
        self.device.launch(gk, &cfg, &args, stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Instancje plastrow zacommitowane dla Bielika (LDY, K) i Mistrala.
    const COMMITTED: &[&str] = &[
        "gemm_fp8_mod_2048x4096_4096",
        "gemm_fp8_mod_1536x4096_4096",
        "gemm_fp8_mod_1024x4096_4096",
        "gemm_fp8_mod_512x4096_4096",
        "gemm_fp8_mod_256x4096_4096",
        "gemm_fp8_mod_2048x4096_11264",
        "gemm_fp8_mod_1536x4096_11264",
        "gemm_fp8_mod_1024x4096_11264",
        "gemm_fp8_mod_512x4096_11264",
        "gemm_fp8_mod_256x4096_11264",
        "gemm_fp8_mod_4096x11264_4096",
        "gemm_fp8_mod_3072x11264_4096",
        "gemm_fp8_mod_2048x11264_4096",
        "gemm_fp8_mod_1536x11264_4096",
        "gemm_fp8_mod_1024x11264_4096",
        "gemm_fp8_mod_512x11264_4096",
        "gemm_fp8_mod_256x11264_4096",
        "gemm_fp8_mod_4096x14336_4096",
        "gemm_fp8_mod_2048x14336_4096",
    ];

    fn plan(rows: usize, cols: usize, n_tokens: usize) -> Option<Vec<usize>> {
        fp8_strip_plan(rows, cols, n_tokens, 48, |name| COMMITTED.contains(&name))
            .map(|p| p.into_iter().map(|(w, _)| w).collect())
    }

    /// Plan musi pokryc dokladnie N — inaczej czesc kolumn zostaje niepoliczona.
    fn assert_covers(rows: usize, widths: &[usize]) {
        assert_eq!(widths.iter().sum::<usize>(), rows);
    }

    #[test]
    fn full_prefill_chunk_reproduces_the_measured_plan() {
        // 1024 tokenow to 8 wierszy M, wiec cel to 256*48/8 = 1536 kolumn.
        // Te trzy plany zmierzyly sie jako 4966 -> 5676 tok/s.
        let qo = plan(4096, 4096, 1024).unwrap();
        assert_eq!(qo, vec![1536, 1536, 1024]);
        let down = plan(4096, 11264, 1024).unwrap();
        assert_eq!(down, vec![1536, 1536, 1024]);
        let gate_up = plan(11264, 4096, 1024).unwrap();
        assert_eq!(
            gate_up,
            vec![1536; 7].into_iter().chain([512]).collect::<Vec<_>>()
        );
        assert_covers(11264, &gate_up);
    }

    #[test]
    fn few_tokens_widen_the_strip() {
        // Przy jednym wierszu M cel jest szerszy niz drabina, wiec schodzimy do
        // jej gornego konca — tego samego, co dawna tabela stalych kawalkow.
        assert_eq!(plan(11264, 4096, 128).unwrap(), vec![4096, 4096, 3072]);
        assert_eq!(
            plan(14336, 4096, 128).unwrap(),
            vec![4096, 4096, 4096, 2048]
        );
    }

    #[test]
    fn more_tokens_narrow_the_strip() {
        // 2048 tokenow to 16 wierszy M, cel 768 — nie ma go w artefaktach, wiec
        // idziemy W GORE do 1024 (64 bloki), nie w dol do 512 (32 bloki na
        // 48 SM, zmierzone 641 ms wobec 362 ms).
        let gate_up = plan(11264, 4096, 2048).unwrap();
        assert_eq!(gate_up[0], 1024);
        assert_covers(11264, &gate_up);
    }

    #[test]
    fn missing_widths_round_up_to_the_committed_ladder() {
        // Mistral ma tylko 4096 i 2048; cel 1536 nie ma pokrycia, wiec bierzemy
        // najblizszy szerszy zamiast wracac do jednego wywolania.
        let gate_up = plan(14336, 4096, 1024).unwrap();
        assert_eq!(gate_up, vec![2048; 7]);
    }

    #[test]
    fn shapes_without_strips_stay_on_the_base_kernel() {
        // K/V (1024 kolumn) nie ma zadnej instancji wezszej od siebie.
        assert!(plan(1024, 4096, 1024).is_none());
        // `down` Mistrala (LDY=4096, K=14336) tez nie.
        assert!(plan(4096, 14336, 1024).is_none());
    }

    #[test]
    fn host_without_sm_count_disables_strips() {
        assert!(fp8_strip_plan(4096, 4096, 1024, 0, |_| true).is_none());
    }
}
