// ===== File: attention.rs — launchery uwagi: prefill, decode, KV, RoPE =====
use super::*;

/// Warps per block in attn_decode (must not exceed MAX_WARPS in attention.mojo).
pub(super) const ATTN_BLOCK: u32 = 128;

#[allow(clippy::too_many_arguments)]
fn validate_attn_prefill_segmented_f16(
    output_bytes: usize,
    q_bytes: usize,
    k_cache_bytes: usize,
    v_cache_bytes: usize,
    page_table_bytes: usize,
    visible_bytes: usize,
    batch: usize,
    n_tokens: usize,
    n_q_heads: usize,
    n_kv_heads: usize,
    head_dim: usize,
    page_size: usize,
    max_pages: usize,
) -> Result<(u32, u32)> {
    if [
        batch, n_tokens, n_q_heads, n_kv_heads, head_dim, page_size, max_pages,
    ]
    .contains(&0)
    {
        return Err(ForgeError::Kernel(
            "segmentowana atencja verifiera wymaga niezerowych wymiarów".into(),
        ));
    }
    if !n_q_heads.is_multiple_of(n_kv_heads) {
        return Err(ForgeError::Kernel(
            "liczba głowic Q segmentowanej atencji musi być wielokrotnością głowic KV".into(),
        ));
    }
    if !matches!(head_dim, 128 | 256) {
        return Err(ForgeError::Kernel(format!(
            "segmentowana atencja obsługuje head_dim 128 albo 256, otrzymano {head_dim}"
        )));
    }
    let total = batch
        .checked_mul(n_tokens)
        .ok_or_else(|| ForgeError::Kernel("przepełnienie liczby tokenów atencji".into()))?;
    let query_bytes = checked_buffer_bytes(
        "segmentowana atencja query/output",
        &[total, n_q_heads, head_dim],
        2,
    )?;
    let required_page_table_bytes =
        checked_buffer_bytes("segmentowana atencja page tables", &[batch, max_pages], 4)?;
    let required_visible_bytes =
        checked_buffer_bytes("segmentowana atencja visible lengths", &[total], 4)?;
    let cache_page_bytes = checked_buffer_bytes(
        "segmentowana atencja strona KV",
        &[n_kv_heads, page_size, head_dim],
        2,
    )?;
    if output_bytes < query_bytes
        || q_bytes < query_bytes
        || page_table_bytes < required_page_table_bytes
        || visible_bytes < required_visible_bytes
        || k_cache_bytes < cache_page_bytes
        || v_cache_bytes < cache_page_bytes
        || k_cache_bytes != v_cache_bytes
        || !k_cache_bytes.is_multiple_of(cache_page_bytes)
    {
        return Err(ForgeError::Kernel(
            "segmentowana atencja verifiera ma za mały lub niezgodny bufor".into(),
        ));
    }
    let grid_x = u32::try_from(total)
        .map_err(|_| ForgeError::Kernel("liczba tokenów atencji przekracza u32".into()))?;
    let grid_y = u32::try_from(n_q_heads)
        .map_err(|_| ForgeError::Kernel("liczba głowic Q przekracza u32".into()))?;
    for (name, value) in [
        ("T", n_tokens),
        ("głowice Q", n_q_heads),
        ("głowice KV", n_kv_heads),
        ("rozmiar strony", page_size),
        ("liczba stron", max_pages),
    ] {
        i64::try_from(value)
            .map_err(|_| ForgeError::Kernel(format!("{name} atencji przekracza i64")))?;
    }
    Ok((grid_x, grid_y))
}

impl Kernels {
    pub fn supports_attn_decode_gqa4_f16_hd128(&self) -> bool {
        self.artifacts.has("attn_decode_split_gqa4_f16_hd128")
            && self.artifacts.has("attn_decode_combine_gqa2_f16_hd128")
    }

    /// Uwaga po zebranych indeksach, z kotwicą w mianowniku softmaxu.
    /// Indeks `-1` oznacza pozycję zamaskowaną.
    #[allow(clippy::too_many_arguments)]
    pub fn sparse_attn_f16(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        kv: &DevBuffer,
        sink: &DevBuffer,
        idxs: &DevBuffer,
        head_dim: usize,
        n_heads: usize,
        n_idx: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("sparse_attn_f16")?;
        // Redukcje idą przez pamięć współdzieloną o stałym rozmiarze 1024.
        let threads = head_dim.next_power_of_two().clamp(64, 1024) as u32;
        let cfg = LaunchConfig {
            grid: (n_heads as u32, 1, 1),
            block: (threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(kv)
            .buf(sink)
            .buf(idxs)
            .scalar(head_dim as i64)
            .scalar(n_heads as i64)
            .scalar(n_idx as i64)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Full (non-paged) attention over contiguous K/V; causal optional.
    /// q/out: [n_q, n_q_heads, hd]; k/v: [n_kv, n_kv_heads, hd].
    #[allow(clippy::too_many_arguments)]
    pub fn attn_full_f16(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_buf: &DevBuffer,
        v_buf: &DevBuffer,
        n_q: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        n_kv: usize,
        causal: bool,
        q_offset: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let name = match head_dim {
            64 => "attn_full_f16_hd64",
            128 => "attn_full_f16_hd128",
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_full: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (n_q as u32, n_q_heads as u32, 1),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_buf)
            .buf(v_buf)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(n_kv as i64)
            .scalar(if causal { 1i64 } else { 0i64 })
            .scalar(q_offset as i64)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Uruchamia przenośną atencję verifiera dla `[B,T]` i osobnych tablic KV.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_verify_segmented_f16_hd256(
        &self,
        output: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_tables: &DevBuffer,
        visible_lens: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.attn_prefill_segmented_f16(
            output,
            q,
            k_cache,
            v_cache,
            page_tables,
            visible_lens,
            batch,
            n_tokens,
            n_q_heads,
            n_kv_heads,
            256,
            page_size,
            max_pages,
            scale,
            stream,
        )
    }

    /// Uruchamia causal prefill dla równych segmentów sequence-major `[B,T]`.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_segmented_f16(
        &self,
        output: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_tables: &DevBuffer,
        visible_lens: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let (grid_x, grid_y) = validate_attn_prefill_segmented_f16(
            output.len(),
            q.len(),
            k_cache.len(),
            v_cache.len(),
            page_tables.len(),
            visible_lens.len(),
            batch,
            n_tokens,
            n_q_heads,
            n_kv_heads,
            head_dim,
            page_size,
            max_pages,
        )?;
        let caps = self.device.caps();
        // Wariant `_warp32` zaklada fale 32 watkow — to jest jego caly wymog.
        // RDNA tez ma fale 32, wiec pytanie o producenta wysylalo Radeony do
        // wariantu pisanego pod fale 64 i dawalo zle wyniki.
        let warp32 = caps.warp_size == 32;
        let kernel_name = if warp32 {
            format!("attn_verify_segmented_f16_hd{head_dim}_warp32")
        } else {
            format!("attn_verify_segmented_f16_hd{head_dim}")
        };
        let kernel = self.artifacts.get(&kernel_name)?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (if warp32 { ATTN_BLOCK } else { 256 }, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_tables)
            .buf(visible_lens)
            .scalar(i64::try_from(n_tokens).expect("T sprawdzone przez validator"))
            .scalar(i64::try_from(n_q_heads).expect("głowice Q sprawdzone przez validator"))
            .scalar(i64::try_from(n_kv_heads).expect("głowice KV sprawdzone przez validator"))
            .scalar(i64::try_from(page_size).expect("rozmiar strony sprawdzony przez validator"))
            .scalar(i64::try_from(max_pages).expect("liczba stron sprawdzona przez validator"))
            .scalar(scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Kafelkowa causal prefill attention dla równych segmentów `[B,T]`.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_segmented_tiled_f16(
        &self,
        output: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_tables: &DevBuffer,
        base_positions: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0
            || n_tokens == 0
            || n_q_heads == 0
            || n_kv_heads == 0
            || !n_q_heads.is_multiple_of(n_kv_heads)
            || !matches!(head_dim, 128 | 256)
            || page_size == 0
            || max_pages == 0
        {
            return Err(ForgeError::Kernel(
                "kafelkowa segmentowana atencja ma nieprawidłowy kształt".into(),
            ));
        }
        let total = batch
            .checked_mul(n_tokens)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie segmentowanego prefill".into()))?;
        let query_bytes = checked_buffer_bytes(
            "segmentowany tiled prefill query",
            &[total, n_q_heads, head_dim],
            2,
        )?;
        let page_table_bytes = checked_buffer_bytes(
            "segmentowany tiled prefill page tables",
            &[batch, max_pages],
            4,
        )?;
        let base_bytes =
            checked_buffer_bytes("segmentowany tiled prefill base positions", &[batch], 4)?;
        let cache_page_bytes = checked_buffer_bytes(
            "segmentowany tiled prefill cache page",
            &[n_kv_heads, page_size, head_dim],
            2,
        )?;
        if output.len() < query_bytes
            || q.len() < query_bytes
            || page_tables.len() < page_table_bytes
            || base_positions.len() < base_bytes
            || k_cache.len() < cache_page_bytes
            || v_cache.len() < cache_page_bytes
            || k_cache.len() != v_cache.len()
            || !k_cache.len().is_multiple_of(cache_page_bytes)
        {
            return Err(ForgeError::Kernel(
                "kafelkowa segmentowana atencja ma za mały lub niezgodny bufor".into(),
            ));
        }
        let tiles_per_sequence = n_tokens.div_ceil(16);
        let grid_x =
            u32::try_from(batch.checked_mul(tiles_per_sequence).ok_or_else(|| {
                ForgeError::Kernel("grid segmentowanego prefill overflow".into())
            })?)
            .map_err(|_| ForgeError::Kernel("grid segmentowanego prefill przekracza u32".into()))?;
        let grid_y = u32::try_from(n_q_heads)
            .map_err(|_| ForgeError::Kernel("głowice prefill przekraczają u32".into()))?;
        let kernel = self
            .artifacts
            .get(&format!("attn_prefill_segmented_f16_hd{head_dim}"))?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_tables)
            .buf(base_positions)
            .scalar(i64::try_from(n_tokens).expect("T segmentowanego prefill jest małe"))
            .scalar(i64::try_from(max_pages).expect("max_pages segmentowanego prefill jest małe"))
            .scalar(i64::try_from(n_q_heads).expect("Q heads segmentowanego prefill są małe"))
            .scalar(i64::try_from(n_kv_heads).expect("KV heads segmentowanego prefill są małe"))
            .scalar(i64::try_from(page_size).expect("page_size segmentowanego prefill jest małe"))
            .scalar(scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Segmentowana FA korzystająca bez zmian z matematyki MMA ścieżki B1.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_fa_segmented_f16_hd128(
        &self,
        output: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_tables: &DevBuffer,
        base_positions: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0
            || n_tokens == 0
            || n_q_heads == 0
            || n_kv_heads == 0
            || !n_q_heads.is_multiple_of(n_kv_heads)
            || page_size == 0
            || max_pages == 0
        {
            return Err(ForgeError::Kernel(
                "segmentowana FA HD128 ma nieprawidłowy kształt".into(),
            ));
        }
        let total = batch
            .checked_mul(n_tokens)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie segmentowanej FA".into()))?;
        let query_bytes =
            checked_buffer_bytes("segmentowana FA query", &[total, n_q_heads, 128], 2)?;
        let page_table_bytes =
            checked_buffer_bytes("segmentowana FA page tables", &[batch, max_pages], 4)?;
        let base_bytes = checked_buffer_bytes("segmentowana FA base positions", &[batch], 4)?;
        let cache_page_bytes = checked_buffer_bytes(
            "segmentowana FA cache page",
            &[n_kv_heads, page_size, 128],
            2,
        )?;
        if output.len() < query_bytes
            || q.len() < query_bytes
            || page_tables.len() < page_table_bytes
            || base_positions.len() < base_bytes
            || k_cache.len() < cache_page_bytes
            || v_cache.len() < cache_page_bytes
            || k_cache.len() != v_cache.len()
            || !k_cache.len().is_multiple_of(cache_page_bytes)
        {
            return Err(ForgeError::Kernel(
                "segmentowana FA HD128 ma za mały lub niezgodny bufor".into(),
            ));
        }
        let kernel = self.artifacts.get("attn_prefill_fa_segmented_f16_hd128")?;
        let config = LaunchConfig {
            grid: (
                u32::try_from(n_tokens.div_ceil(64))
                    .map_err(|_| ForgeError::Kernel("grid.x FA przekracza u32".into()))?,
                u32::try_from(n_q_heads)
                    .map_err(|_| ForgeError::Kernel("grid.y FA przekracza u32".into()))?,
                u32::try_from(batch)
                    .map_err(|_| ForgeError::Kernel("grid.z FA przekracza u32".into()))?,
            ),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_tables)
            .buf(base_positions)
            .scalar(i64::try_from(n_tokens).expect("T segmentowanej FA jest małe"))
            .scalar(i64::try_from(max_pages).expect("max_pages segmentowanej FA jest małe"))
            .scalar(i64::try_from(n_q_heads).expect("Q heads segmentowanej FA są małe"))
            .scalar(i64::try_from(n_kv_heads).expect("KV heads segmentowanej FA są małe"))
            .scalar(i64::try_from(page_size).expect("page_size segmentowanej FA jest małe"))
            .scalar(scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Causal prefill attention over the paged cache. Query token t attends
    /// positions 0..base_pos+t, whose K/V must already be appended.
    /// `kv_dtype` selects the cache element type; the fp8 variant widens
    /// e4m3 rows to f16 in shared memory (exact), so its math matches the
    /// f16 kernel on a dequantized cache bit-for-bit.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        kv_dtype: DType,
        scale: f32,
        // Okno przesuwne w tokenach; 0 = pełna uwaga przyczynowa.
        window: usize,
        stream: &Stream,
    ) -> Result<()> {
        // Tensor-core flash-attention paths. Only the f16 cache with head_dim
        // 64/128 has an FA specialization; every other shape falls through to
        // the Mojo scalar kernel so nothing breaks.
        if kv_dtype == DType::F16 && (head_dim == 64 || head_dim == 128) {
            match self.attn {
                AttnBackend::Cuda => {
                    return self.attn_prefill_fa(
                        out, q, k_cache, v_cache, page_table, base_pos, n_tokens, n_q_heads,
                        n_kv_heads, head_dim, page_size, scale, stream, false,
                    );
                }
                AttnBackend::Mojo => {
                    return self.attn_prefill_fa(
                        out, q, k_cache, v_cache, page_table, base_pos, n_tokens, n_q_heads,
                        n_kv_heads, head_dim, page_size, scale, stream, true,
                    );
                }
                AttnBackend::Scalar => {}
            }
        }
        // RDNA4: flash attention na jednostce macierzowej. Kafel 16x16 liczy
        // Q·Kᵀ i P·V przez WMMA zamiast iloczynów skalarnych na linię —
        // zmierzone na R9700 (32 głowice Q, 8 KV, head_dim 128):
        //   T=512  738 us -> 327 us (2,25x)
        //   T=1024 2703 us -> 899 us (3,00x)
        //   T=2048 8508 us -> 2591 us (3,28x)
        // Przewaga rośnie z długością sekwencji, bo koszt uwagi rośnie
        // kwadratowo. Kernel nie obsługuje okna przesuwnego ani cache'u innego
        // niż f16, więc te przypadki idą dalej starą ścieżką.
        //
        // `head_dim` NALEŻY DO MODELU, więc kernel ma je jako parametr
        // kompilacji, a nie jako stałą — wybór idzie po `head_dim` z deskryptora
        // i po obecności artefaktu tej instancji. Dołożenie kolejnego kształtu
        // to alias w `prefill_wmma.mojo` plus wpis w katalogu; nic tutaj nie
        // trzeba zmieniać i żaden inny model na tym nie ucierpi.
        let wmma_variant = match head_dim {
            128 => Some("attn_prefill_wmma_hd128"),
            256 => Some("attn_prefill_wmma_hd256"),
            _ => None,
        }
        .filter(|name| self.artifacts.has(name));
        if kv_dtype == DType::F16 && window == 0 && wmma_variant.is_some() {
            let k = self
                .artifacts
                .get(wmma_variant.expect("wariant sprawdzony"))?;
            let cfg = LaunchConfig {
                grid: ((n_tokens as u32).div_ceil(64), n_q_heads as u32, 1),
                block: (128, 1, 1),
                shared_mem_bytes: 0,
            };
            let args = LaunchArgs::new()
                .buf(out)
                .buf(q)
                .buf(k_cache)
                .buf(v_cache)
                .buf(page_table)
                .scalar(base_pos as i64)
                .scalar(n_q_heads as i64)
                .scalar(n_kv_heads as i64)
                .scalar(page_size as i64)
                .scalar(scale)
                .scalar(n_tokens as i64);
            return self.device.launch(k, &cfg, &args, stream);
        }
        let suffix = Self::kv_suffix(kv_dtype, "attn_prefill")?;
        let name = match (head_dim, kv_dtype) {
            (64, _) => format!("attn_prefill_{suffix}_hd64"),
            (128, _) => format!("attn_prefill_{suffix}_hd128"),
            // Only the f16 cache has an hd256 specialization (qwen35moe
            // attention layers); fp8/rot hd256 is not compiled.
            (256, DType::F16) => format!("attn_prefill_{suffix}_hd256"),
            // 512: warstwy globalne Gemmy 4. Kafel pozycji jest tam o połowę
            // mniejszy (LDS), ale kontrakt uruchomienia jest ten sam.
            (512, DType::F16) => format!("attn_prefill_{suffix}_hd512"),
            (other, _) => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_prefill: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(&name)?;
        // Kernel tiling contract (prefill.mojo QT): 16 queries per block,
        // block of 8 warps.
        let cfg = LaunchConfig {
            grid: ((n_tokens as u32).div_ceil(16), n_q_heads as u32, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(scale)
            .scalar(n_tokens as i64)
            .scalar(window as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Wykonuje prefill HD256 z pozycją bazową przechowywaną na urządzeniu.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_device_pos_f16_hd256(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: &DevBuffer,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        // RDNA4 liczy ten sam kafel na jednostce macierzowej. Kontrakt wywołania
        // jest identyczny — różni się tylko rozkład pracy: 64 zapytania na blok
        // czterema falami zamiast 16 zapytań ośmioma. Bez artefaktu zostaje
        // ścieżka skalarna, więc pozostałe karty nic nie tracą.
        let (k, per_block, threads) = match self.artifacts.get("attn_prefill_wmma_pos_hd256") {
            Ok(k) => (k, 64u32, 128u32),
            Err(_) => (
                self.artifacts.get("attn_prefill_device_pos_f16_hd256")?,
                16u32,
                256u32,
            ),
        };
        let cfg = LaunchConfig {
            grid: ((n_tokens as u32).div_ceil(per_block), n_q_heads as u32, 1),
            block: (threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(base_pos)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(scale)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Uruchamia Mojo Flash Attention HD256 ze zwalidowaną pozycją bazową.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_fa_mojo_f16_hd256(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        base_position: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let (grid_x, grid_y) = validate_attn_prefill_fa_f16_hd256(
            out.len(),
            q.len(),
            k_cache.len(),
            v_cache.len(),
            page_table.len(),
            base_position,
            n_tokens,
            n_q_heads,
            n_kv_heads,
            page_size,
            scale,
        )?;
        if self.device.caps().max_threads_per_block < 128 {
            return Err(ForgeError::Unsupported(
                "flash attention prefill HD256 wymaga bloku 128 wątków".into(),
            ));
        }
        let experimental_bk32 =
            std::env::var("FORGE_HYBRID_FA_KEY_TILE").is_ok_and(|value| value == "32");
        let kernel_name =
            if experimental_bk32 && self.artifacts.has("attn_prefill_fa_mojo_f16_hd256_bk32") {
                "attn_prefill_fa_mojo_f16_hd256_bk32"
            } else if self.artifacts.has("attn_prefill_fa_mojo_f16_hd256_vtrans") {
                "attn_prefill_fa_mojo_f16_hd256_vtrans"
            } else {
                "attn_prefill_fa_mojo_f16_hd256"
            };
        let kernel = self.artifacts.get(kernel_name)?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .scalar(
                i64::try_from(base_position).expect("pozycja bazowa sprawdzona przez validator"),
            )
            .scalar(i64::try_from(n_q_heads).expect("głowice Q sprawdzone przez validator"))
            .scalar(i64::try_from(n_kv_heads).expect("głowice KV sprawdzone przez validator"))
            .scalar(i64::try_from(page_size).expect("rozmiar strony sprawdzony przez validator"))
            .scalar(scale)
            .scalar(i64::try_from(n_tokens).expect("T sprawdzone przez validator"));
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Tensor-core causal flash-attention prefill. Same I/O contract as
    /// `attn_prefill` (f16 cache, paged KV, GQA, causal) but QK^T and P·V run as
    /// f16 mma with an online softmax kept in registers. Grid: (ceil(T/64),
    /// n_q_heads); one block of 4 warps owns 64 query rows of one head. `mojo`
    /// selects the portable Mojo kernel (`attn_prefill_fa_mma`,
    /// kernels/mojo/src/prefill.mojo) over the CUDA cubin
    /// (kernels/cuda/fattn_prefill.cu) — byte-identical tiling contract.
    #[allow(clippy::too_many_arguments)]
    fn attn_prefill_fa(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        scale: f32,
        stream: &Stream,
        mojo: bool,
    ) -> Result<()> {
        let name = match (head_dim, mojo) {
            (64, false) => "attn_prefill_fa_f16_hd64",
            (128, false) => "attn_prefill_fa_f16_hd128",
            (64, true) => "attn_prefill_fa_mojo_f16_hd64",
            (128, true) => "attn_prefill_fa_mojo_f16_hd128",
            (other, _) => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_prefill_fa: head_dim {other} has no FA specialization"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        // Kernel tiling contract (fattn_prefill.cu): BQ=64 queries per block,
        // 4 warps = 128 threads.
        let cfg = LaunchConfig {
            grid: ((n_tokens as u32).div_ceil(64), n_q_heads as u32, 1),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(scale)
            .scalar(n_tokens as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Paged flash-decode attention. Layouts documented in attention.mojo.
    /// Wartości długości sekwencji i fizyczne identyfikatory stron są
    /// przygotowywane oraz walidowane przez właściciela cache przed wywołaniem.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_f16(
        &self,
        out: &DevBuffer,
        parts: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        // Okno przesuwne w tokenach; 0 = pełny kontekst.
        window: usize,
        stream: &Stream,
    ) -> Result<()> {
        let caps = self.device.caps();
        // Wariant split8 nie ma jeszcze maskowania okna, więc przy oknie
        // schodzimy na ścieżkę generyczną, która je obsługuje.
        // Wariant dzielony maskuje już okno przesuwne, więc obowiązuje także
        // dla warstw okiennych (40 z 48 warstw Gemmy 4).
        let split_suffix = match head_dim {
            64 => "hd64",
            128 => "hd128",
            512 => "hd512",
            _ => "hd256",
        };
        let split8_available = self
            .artifacts
            .has(&format!("attn_decode_split8_f16_{split_suffix}"))
            && self
                .artifacts
                .has(&format!("attn_decode_split8_combine_f16_{split_suffix}"));
        let plan = attn_decode_plan(
            head_dim,
            caps.warp_size,
            caps.max_threads_per_block,
            split8_available,
        )?;
        let (grid_x, grid_y) = validate_attn_decode_f16(
            out.len(),
            parts.len(),
            q.len(),
            k_cache.len(),
            v_cache.len(),
            page_table.len(),
            seq_lens.len(),
            n_seqs,
            n_q_heads,
            n_kv_heads,
            head_dim,
            page_size,
            max_pages,
            scale,
            !matches!(plan, AttnDecodePlan::Generic(_)),
        )?;
        if !matches!(plan, AttnDecodePlan::Generic(_)) {
            // Wariant rozdzielny czyta cache KV RAZ NA GŁOWICĘ Q, więc przy GQA
            // przemiata go tyle razy, ile głowic ma grupa. Wariant dzielony ma tę
            // samą matematykę i ten sam układ partiali, tylko siatkę po głowicach
            // KV — zmierzone na 32k kontekstu: 22 ms na token w samym dekodzie
            // uwagi, czyli ~95 GB/s przy 551 GB/s osiągalnych.
            let group = (n_q_heads / n_kv_heads.max(1)).min(n_q_heads);
            let shared = (head_dim == 256 && n_q_heads.is_multiple_of(n_kv_heads.max(1)))
                .then(|| format!("attn_decode_split_gqa{group}_f16_hd256"))
                .filter(|name| self.artifacts.has(name));
            let partial = match &shared {
                Some(name) => self.artifacts.get(name)?,
                None => self
                    .artifacts
                    .get(&format!("attn_decode_split8_f16_{split_suffix}"))?,
            };
            let partial_config = LaunchConfig {
                // Siatka wariantu dzielonego idzie po głowicach KV, bo jedna grupa
                // robocza obsługuje całą grupę Q.
                grid: (
                    grid_x,
                    match shared {
                        Some(_) => (n_kv_heads as u32).max(1),
                        None => grid_y,
                    },
                    ATTN_HD256_SPLITS as u32,
                ),
                block: (ATTN_HD256_BLOCK, 1, 1),
                shared_mem_bytes: 0,
            };
            let partial_args = LaunchArgs::new()
                .buf(parts)
                .buf(q)
                .buf(k_cache)
                .buf(v_cache)
                .buf(page_table)
                .buf(seq_lens)
                .scalar(n_q_heads as i64)
                .scalar(n_kv_heads as i64)
                .scalar(page_size as i64)
                .scalar(max_pages as i64)
                .scalar(scale)
                .scalar(window as i64);
            self.device
                .launch(partial, &partial_config, &partial_args, stream)?;

            let combine = self
                .artifacts
                .get(&format!("attn_decode_split8_combine_f16_{split_suffix}"))?;
            let combine_config = LaunchConfig {
                grid: (grid_x, grid_y, 1),
                block: (32, 1, 1),
                shared_mem_bytes: 0,
            };
            let combine_args = LaunchArgs::new()
                .buf(out)
                .buf(parts)
                .scalar(n_q_heads as i64);
            return self
                .device
                .launch(combine, &combine_config, &combine_args, stream);
        }
        let name = match plan {
            AttnDecodePlan::Generic(name) => name,
            _ => unreachable!("wariant dzielony zwraca wynik przed fallbackiem"),
        };
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(seq_lens)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(max_pages as i64)
            .scalar(scale)
            .scalar(window as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Dokładny batch flash-decode korzystający ze wspólnej tablicy stron.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_batch_exact_f16_hd256(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let kernel = self.artifacts.get("attn_decode_batch_exact_f16_hd256")?;
        let config = LaunchConfig {
            grid: (n_tokens as u32, n_q_heads as u32, 1),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(seq_lens)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(max_pages as i64)
            .scalar(scale);
        self.device.launch(kernel, &config, &args, stream)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn attn_verify_split8_f16_hd256(
        &self,
        out: &DevBuffer,
        parts: &DevBuffer,
        q: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        max_pages: usize,
        scale: f32,
        stream: &Stream,
    ) -> Result<bool> {
        let caps = self.device.caps();
        // Kernel jest przenośny — `warp.sum`, bariery i LDS, zero intrinsiców
        // producenta — więc jedynym realnym wymogiem jest fala 32 i pojemność
        // LDS. Bramka na vendora zostawiała RDNA4 na kaflu PREFILLOWYM, który
        // zrównolegla po TOKENACH: przy T=4 to 24 grupy robocze na 64 CU, każda
        // szeregowo przez cały kontekst. Zmierzone na R9700, kontekst 4672:
        // 2,28 ms na warstwę razy 16 warstw = 36,5 ms na krok weryfikacji.
        if !verify_attn_split8_enabled(std::env::var("FORGE_VERIFY_ATTN_SPLIT8").ok().as_deref())
            || !matches!(n_tokens, 3 | 4)
            || caps.warp_size != 32
            || caps.max_threads_per_block < 256
            || caps.max_shared_mem_per_block < 33_024
        {
            return Ok(false);
        }
        let partial_name = format!("attn_verify_split8_f16_hd256_t{n_tokens}");
        let combine_name = "attn_verify_split8_combine_f16_hd256";
        if !self.artifacts.has(&partial_name) || !self.artifacts.has(combine_name) {
            return Ok(false);
        }
        let (grid_y, combine_grid_y) = validate_attn_verify_split8(
            out.len(),
            parts.len(),
            q.len(),
            k_cache.len(),
            v_cache.len(),
            page_table.len(),
            seq_lens.len(),
            n_tokens,
            n_q_heads,
            n_kv_heads,
            page_size,
            max_pages,
            scale,
        )?;
        let partial = self.artifacts.get(&partial_name)?;
        let args = LaunchArgs::new()
            .buf(parts)
            .buf(q)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(seq_lens)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(max_pages as i64)
            .scalar(scale);
        self.device.launch(
            partial,
            &LaunchConfig {
                grid: (1, grid_y, 8),
                block: (256, 1, 1),
                shared_mem_bytes: 0,
            },
            &args,
            stream,
        )?;
        let combine = self.artifacts.get(combine_name)?;
        let args = LaunchArgs::new()
            .buf(out)
            .buf(parts)
            .scalar(n_tokens as i64)
            .scalar(n_q_heads as i64);
        self.device.launch(
            combine,
            &LaunchConfig {
                grid: (1, combine_grid_y, 1),
                block: (32, 1, 1),
                shared_mem_bytes: 0,
            },
            &args,
            stream,
        )?;
        Ok(true)
    }

    /// Split-context flash-decode attention with the qkv_post stage fused in
    /// as a per-block prologue (q/k RMSNorm + RoPE + paged k/v append). q/k/v
    /// are sections of the raw QKV GEMV output addressed by byte offsets;
    /// rotated q lives only in shared memory (the q section is never written
    /// back). Unnormalized per-split partials land in `parts`
    /// ([n_seqs, n_q_heads, n_splits, head_dim + 2] f32) for
    /// attn_decode_combine_f16. n_splits == 1 is bit-exact vs attn_decode_f16.
    /// `kv_dtype` selects the cache element type: the fp8 variant appends
    /// e4m3(f16(rope(k)))/e4m3(v) and widens cache reads exactly, so its
    /// math matches the f16 kernel on a dequantized cache bit-for-bit.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_split(
        &self,
        parts: &DevBuffer,
        q_in: &DevBuffer,
        q_byte_off: usize,
        k_in: &DevBuffer,
        k_byte_off: usize,
        v_in: &DevBuffer,
        v_byte_off: usize,
        q_norm: Option<&DevBuffer>,
        k_norm: Option<&DevBuffer>,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        positions: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        max_pages: usize,
        n_splits: usize,
        kv_dtype: DType,
        eps: f32,
        theta_base: f32,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let suffix = Self::kv_suffix(kv_dtype, "attn_decode_split")?;
        let name = match head_dim {
            64 => format!("attn_decode_split_{suffix}_hd64"),
            128 => format!("attn_decode_split_{suffix}_hd128"),
            // 512: warstwy globalne Gemmy 4.
            512 => format!("attn_decode_split_{suffix}_hd512"),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_decode_split: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, n_q_heads as u32, n_splits as u32),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        // Absent norm weights are flagged off; the pointer slot still needs a
        // valid device address, so q_in stands in (never dereferenced).
        let args = LaunchArgs::new()
            .buf(parts)
            .buf_at(q_in, q_byte_off)?
            .buf_at(k_in, k_byte_off)?
            .buf_at(v_in, v_byte_off)?
            .buf(q_norm.unwrap_or(q_in))
            .buf(k_norm.unwrap_or(q_in))
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(seq_lens)
            .buf(positions)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(max_pages as i64)
            .scalar(n_splits as i64)
            .scalar(if q_norm.is_some() { 1i64 } else { 0i64 })
            .scalar(if k_norm.is_some() { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(theta_base)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Split attention F16 dla GQA 4:1, współdzielący odczyt K/V między głowicami Q.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_split_gqa4_f16_hd128(
        &self,
        parts: &DevBuffer,
        q_in: &DevBuffer,
        q_byte_off: usize,
        k_in: &DevBuffer,
        k_byte_off: usize,
        v_in: &DevBuffer,
        v_byte_off: usize,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        positions: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        page_size: usize,
        max_pages: usize,
        n_splits: usize,
        eps: f32,
        theta_base: f32,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let expected_q_heads = n_kv_heads.checked_mul(4).ok_or_else(|| {
            ForgeError::Kernel("attn_decode_split_gqa4: liczba głowic przekracza zakres".into())
        })?;
        if n_seqs == 0
            || n_q_heads == 0
            || n_kv_heads == 0
            || n_q_heads != expected_q_heads
            || page_size == 0
            || max_pages == 0
            || n_splits == 0
        {
            return Err(ForgeError::Kernel(format!(
                "attn_decode_split_gqa4 wymaga niezerowych wymiarów i GQA 4:1, otrzymano seqs={n_seqs}, heads={n_q_heads}:{n_kv_heads}, page={page_size}, max_pages={max_pages}, splits={n_splits}"
            )));
        }
        if !q_byte_off.is_multiple_of(2)
            || !k_byte_off.is_multiple_of(2)
            || !v_byte_off.is_multiple_of(2)
        {
            return Err(ForgeError::Kernel(
                "attn_decode_split_gqa4 wymaga offsetów wyrównanych do F16".into(),
            ));
        }
        let parts_bytes = checked_buffer_bytes(
            "attn_decode_split_gqa4 parts",
            &[n_seqs, n_q_heads, n_splits, 130],
            4,
        )?;
        let q_bytes =
            checked_buffer_bytes("attn_decode_split_gqa4 q", &[n_seqs, n_q_heads, 128], 2)?;
        let kv_bytes =
            checked_buffer_bytes("attn_decode_split_gqa4 kv", &[n_seqs, n_kv_heads, 128], 2)?;
        let cache_page_bytes = checked_buffer_bytes(
            "attn_decode_split_gqa4 cache",
            &[n_kv_heads, page_size, 128],
            2,
        )?;
        let page_table_bytes =
            checked_buffer_bytes("attn_decode_split_gqa4 page_table", &[n_seqs, max_pages], 4)?;
        let metadata_bytes = checked_buffer_bytes("attn_decode_split_gqa4 metadata", &[n_seqs], 4)?;
        let q_end = q_byte_off.checked_add(q_bytes).ok_or_else(|| {
            ForgeError::Kernel("attn_decode_split_gqa4: przepełnienie zakresu Q".into())
        })?;
        let k_end = k_byte_off.checked_add(kv_bytes).ok_or_else(|| {
            ForgeError::Kernel("attn_decode_split_gqa4: przepełnienie zakresu K".into())
        })?;
        let v_end = v_byte_off.checked_add(kv_bytes).ok_or_else(|| {
            ForgeError::Kernel("attn_decode_split_gqa4: przepełnienie zakresu V".into())
        })?;
        if parts.len() < parts_bytes
            || q_in.len() < q_end
            || k_in.len() < k_end
            || v_in.len() < v_end
            || k_cache.len() < cache_page_bytes
            || v_cache.len() < cache_page_bytes
            || page_table.len() < page_table_bytes
            || seq_lens.len() < metadata_bytes
            || positions.len() < metadata_bytes
        {
            return Err(ForgeError::Kernel(
                "attn_decode_split_gqa4: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let grid_x = u32::try_from(n_seqs).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_seqs przekracza zakres siatki".into())
        })?;
        let grid_y = u32::try_from(n_kv_heads).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_kv_heads przekracza zakres siatki".into())
        })?;
        let grid_z = u32::try_from(n_splits).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_splits przekracza zakres siatki".into())
        })?;
        let n_q_heads_i64 = i64::try_from(n_q_heads).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_q_heads przekracza ABI Mojo".into())
        })?;
        let n_kv_heads_i64 = i64::try_from(n_kv_heads).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_kv_heads przekracza ABI Mojo".into())
        })?;
        let page_size_i64 = i64::try_from(page_size).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: page_size przekracza ABI Mojo".into())
        })?;
        let max_pages_i64 = i64::try_from(max_pages).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: max_pages przekracza ABI Mojo".into())
        })?;
        let n_splits_i64 = i64::try_from(n_splits).map_err(|_| {
            ForgeError::Kernel("attn_decode_split_gqa4: n_splits przekracza ABI Mojo".into())
        })?;
        let k = self.artifacts.get("attn_decode_split_gqa4_f16_hd128")?;
        let cfg = LaunchConfig {
            grid: (grid_x, grid_y, grid_z),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(parts)
            .buf_at(q_in, q_byte_off)?
            .buf_at(k_in, k_byte_off)?
            .buf_at(v_in, v_byte_off)?
            .buf(q_in)
            .buf(k_in)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .buf(seq_lens)
            .buf(positions)
            .scalar(n_q_heads_i64)
            .scalar(n_kv_heads_i64)
            .scalar(page_size_i64)
            .scalar(max_pages_i64)
            .scalar(n_splits_i64)
            .scalar(0i64)
            .scalar(0i64)
            .scalar(eps)
            .scalar(theta_base)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Merge attn_decode_split_f16 partials into the final [n_seqs,
    /// n_q_heads, head_dim] f16 output (one warp per head, split order).
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_combine_f16(
        &self,
        out: &DevBuffer,
        parts: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        head_dim: usize,
        n_splits: usize,
        stream: &Stream,
    ) -> Result<()> {
        let name = match head_dim {
            64 => "attn_decode_combine_f16_hd64",
            128 => "attn_decode_combine_f16_hd128",
            512 => "attn_decode_combine_f16_hd512",
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_decode_combine: head_dim {other} has no compiled specialization"
                )))
            }
        };
        let k = self.artifacts.get(name)?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, n_q_heads as u32, 1),
            block: (32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(parts)
            .scalar(n_q_heads as i64)
            .scalar(n_splits as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Laczy partiale GQA hd128, przetwarzajac dwie glowice Q w jednym CTA.
    pub fn attn_decode_combine_gqa2_f16_hd128(
        &self,
        out: &DevBuffer,
        parts: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_splits: usize,
        stream: &Stream,
    ) -> Result<()> {
        if n_seqs == 0 || n_q_heads == 0 || n_splits == 0 {
            return Err(ForgeError::Kernel(
                "attn_decode_combine_gqa2 wymaga niezerowych wymiarów".into(),
            ));
        }
        let out_bytes =
            checked_buffer_bytes("attn_decode_combine_gqa2 out", &[n_seqs, n_q_heads, 128], 2)?;
        let parts_bytes = checked_buffer_bytes(
            "attn_decode_combine_gqa2 parts",
            &[n_seqs, n_q_heads, n_splits, 130],
            4,
        )?;
        if out.len() < out_bytes || parts.len() < parts_bytes {
            return Err(ForgeError::Kernel(
                "attn_decode_combine_gqa2: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let grid_x = u32::try_from(n_seqs).map_err(|_| {
            ForgeError::Kernel("attn_decode_combine_gqa2: n_seqs przekracza zakres siatki".into())
        })?;
        let grid_y = u32::try_from(n_q_heads.div_ceil(2)).map_err(|_| {
            ForgeError::Kernel(
                "attn_decode_combine_gqa2: n_q_heads przekracza zakres siatki".into(),
            )
        })?;
        let n_q_heads_i64 = i64::try_from(n_q_heads).map_err(|_| {
            ForgeError::Kernel("attn_decode_combine_gqa2: n_q_heads przekracza ABI Mojo".into())
        })?;
        let n_splits_i64 = i64::try_from(n_splits).map_err(|_| {
            ForgeError::Kernel("attn_decode_combine_gqa2: n_splits przekracza ABI Mojo".into())
        })?;
        let k = self.artifacts.get("attn_decode_combine_gqa2_f16_hd128")?;
        let cfg = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (64, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(parts)
            .scalar(n_q_heads_i64)
            .scalar(n_splits_i64);
        self.device.launch(k, &cfg, &args, stream)
    }

}
