// ===== File: attention_kv.rs — RoPE, zapisy cache'u K/V i ścieżka rot =====
use super::*;
use super::attention::ATTN_BLOCK;

impl Kernels {
    /// In-place neox RoPE over [n_tokens, n_heads, head_dim] f16.
    #[allow(clippy::too_many_arguments)]
    pub fn rope_neox_f16(
        &self,
        x_io: &DevBuffer,
        positions: &DevBuffer,
        n_tokens: usize,
        n_heads: usize,
        head_dim: usize,
        theta_base: f32,
        freq_factors: Option<&DevBuffer>,
        stream: &Stream,
    ) -> Result<()> {
        if theta_base == 0.0 {
            return Ok(());
        }
        let k = self.artifacts.get(match freq_factors {
            // Rope proporcjonalne (warstwy globalne Gemmy) dzieli częstotliwość
            // każdej pary przez współczynnik z tensora `rope_freqs`.
            Some(_) => "rope_neox_ff_f16",
            None => "rope_neox_f16",
        })?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_heads as u32, 1),
            block: ((head_dim as u32 / 2).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let mut args = LaunchArgs::new().buf(x_io).buf(positions);
        if let Some(ff) = freq_factors {
            args = args.buf(ff);
        }
        let args = args
            .scalar(n_heads as i64)
            .scalar(head_dim as i64)
            .scalar(theta_base);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `rope_neox_f16` over a section of a fused buffer, addressed by byte
    /// offset. Used by the rot decode path to rope the q/k slices of a fused
    /// qkv buffer in place.
    #[allow(clippy::too_many_arguments)]
    pub fn rope_neox_f16_at(
        &self,
        x_io: &DevBuffer,
        byte_off: usize,
        positions: &DevBuffer,
        n_tokens: usize,
        n_heads: usize,
        head_dim: usize,
        theta_base: f32,
        freq_factors: Option<&DevBuffer>,
        stream: &Stream,
    ) -> Result<()> {
        if theta_base == 0.0 {
            return Ok(());
        }
        let k = self.artifacts.get(match freq_factors {
            // Rope proporcjonalne (warstwy globalne Gemmy) dzieli częstotliwość
            // każdej pary przez współczynnik z tensora `rope_freqs`.
            Some(_) => "rope_neox_ff_f16",
            None => "rope_neox_f16",
        })?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_heads as u32, 1),
            block: ((head_dim as u32 / 2).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let mut args = LaunchArgs::new().buf_at(x_io, byte_off)?.buf(positions);
        if let Some(ff) = freq_factors {
            args = args.buf(ff);
        }
        let args = args
            .scalar(n_heads as i64)
            .scalar(head_dim as i64)
            .scalar(theta_base);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Partial NEOX rotary: rotate only the first `n_rot` dims of each head
    /// (qwen35moe M-RoPE reduces to this for text positions). Layout matches
    /// `rope_neox_f16` ([n_tokens, n_heads, head_dim], in place).
    #[allow(clippy::too_many_arguments)]
    pub fn rope_neox_partial_f16(
        &self,
        x_io: &DevBuffer,
        positions: &DevBuffer,
        n_tokens: usize,
        n_heads: usize,
        head_dim: usize,
        n_rot: usize,
        theta_base: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rope_neox_partial_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_heads as u32, 1),
            block: ((n_rot as u32 / 2).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(x_io)
            .buf(positions)
            .scalar(n_heads as i64)
            .scalar(head_dim as i64)
            .scalar(n_rot as i64)
            .scalar(theta_base);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Czy scalony wstęp warstwy uwagi ma artefakt i pasuje do geometrii.
    /// Blok ma `head_dim` wątków i mieści redukcję normy, a `n_rot` musi być
    /// parzyste, bo RoPE łączy indeksy `j` i `j + n_rot/2`.
    pub fn attn_prepare_qk_capable(&self, head_dim: usize, n_rot: usize) -> bool {
        self.artifacts.get("attn_prepare_qk_f16").is_ok()
            && head_dim > 0
            && head_dim <= 1024
            && n_rot > 0
            && n_rot.is_multiple_of(2)
            && n_rot <= head_dim
    }

    /// Rozplecenie bramkowanej projekcji Q, obie normy głowic i oba częściowe
    /// RoPE jednym uruchomieniem. Zastępuje pięć uruchomień na warstwę uwagi.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prepare_qk_f16(
        &self,
        qc: &DevBuffer,
        gatec: &DevBuffer,
        k_io: &DevBuffer,
        q_full: &DevBuffer,
        q_norm: &DevBuffer,
        k_norm: &DevBuffer,
        positions: &DevBuffer,
        n_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        n_rot: usize,
        theta_base: f32,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if !self.attn_prepare_qk_capable(head_dim, n_rot) {
            return Err(ForgeError::Kernel(
                "attn_prepare_qk_f16: brak artefaktu albo geometria poza kontraktem".into(),
            ));
        }
        let q_span = checked_buffer_bytes("attn_prepare_qk q", &[n_heads, head_dim], 2)?;
        let kv_span = checked_buffer_bytes("attn_prepare_qk kv", &[n_kv_heads, head_dim], 2)?;
        if qc.len() < q_span
            || gatec.len() < q_span
            || k_io.len() < kv_span
            || q_full.len() < 2 * q_span
        {
            return Err(ForgeError::Kernel(
                "attn_prepare_qk_f16: bufor jest mniejszy od kształtu głowic".into(),
            ));
        }
        let k = self.artifacts.get("attn_prepare_qk_f16")?;
        let cfg = LaunchConfig {
            grid: ((n_heads + n_kv_heads) as u32, 1, 1),
            block: (head_dim as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(qc)
            .buf(gatec)
            .buf(k_io)
            .buf(q_full)
            .buf(q_norm)
            .buf(k_norm)
            .buf(positions)
            .scalar(n_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(head_dim as i64)
            .scalar(n_rot as i64)
            .scalar(theta_base)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Rope obracające pary sąsiadujące `(2i, 2i+1)` na wycinku każdego wiersza.
    /// `inverse` sprzęga obrót — tak rope wchodzi na wyjście uwagi.
    #[allow(clippy::too_many_arguments)]
    pub fn rope_interleaved_f16(
        &self,
        buf: &DevBuffer,
        freqs: &DevBuffer,
        row_stride: usize,
        offset: usize,
        rope_dim: usize,
        n_rows: usize,
        pos_base: usize,
        pos_stride: usize,
        inverse: bool,
        stream: &Stream,
    ) -> Result<()> {
        if !rope_dim.is_multiple_of(2) {
            return Err(ForgeError::Kernel(format!(
                "rope_interleaved wymaga parzystego rope_dim, otrzymano {rope_dim}"
            )));
        }
        let k = self.artifacts.get("rope_interleaved_f16")?;
        let total = n_rows * (rope_dim / 2);
        let cfg = LaunchConfig {
            grid: ((total as u32).div_ceil(BLOCK), 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(buf)
            .buf(freqs)
            .scalar(row_stride as i64)
            .scalar(offset as i64)
            .scalar(rope_dim as i64)
            .scalar(n_rows as i64)
            .scalar(pos_base as i64)
            .scalar(pos_stride as i64)
            .scalar(i64::from(inverse));
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Scatter the current token's K/V rows ([n_kv_heads, head_dim]) into the
    /// paged cache at position seq_len[0]-1 (device-resident addressing —
    /// CUDA-graph-replay safe).
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_f16(
        &self,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        k_in: &DevBuffer,
        v_in: &DevBuffer,
        page_table: &DevBuffer,
        seq_len: &DevBuffer,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("kv_append_f16")?;
        let cfg = LaunchConfig {
            grid: (n_kv_heads as u32, 1, 1),
            block: ((head_dim as u32).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_cache)
            .buf(v_cache)
            .buf(k_in)
            .buf(v_in)
            .buf(page_table)
            .buf(seq_len)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(head_dim as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused decode QKV post-processing: optional per-head q/k RMSNorm, neox
    /// RoPE on q and k, and the paged-cache k/v append in ONE launch. q/k/v
    /// are [heads, head_dim] rows addressed by byte offsets (sections of a
    /// fused qkv buffer or separate buffers). Position and page id come from
    /// device buffers — CUDA-graph-replay safe. Bit-exact vs the separate
    /// rmsnorm/rope/kv_append chain (verified in test_kernels.mojo).
    #[allow(clippy::too_many_arguments)]
    pub fn qkv_post_f16(
        &self,
        q_io: &DevBuffer,
        q_byte_off: usize,
        k_in: &DevBuffer,
        k_byte_off: usize,
        v_in: &DevBuffer,
        v_byte_off: usize,
        q_norm: Option<&DevBuffer>,
        k_norm: Option<&DevBuffer>,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        positions: &DevBuffer,
        page_table: &DevBuffer,
        seq_len: &DevBuffer,
        n_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        eps: f32,
        theta_base: f32,
        stream: &Stream,
    ) -> Result<()> {
        // One element per thread: block = head_dim (MAX_HEAD_DIM in
        // qkv_post.mojo bounds the shared staging array).
        if head_dim > 256 || !head_dim.is_multiple_of(32) {
            return Err(ForgeError::Kernel(format!(
                "qkv_post requires head_dim % 32 == 0 and head_dim <= 256, got {head_dim}"
            )));
        }
        let k = self.artifacts.get("qkv_post_f16")?;
        let cfg = LaunchConfig {
            grid: ((n_heads + n_kv_heads) as u32, 1, 1),
            block: (head_dim as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        // Absent norm weights are flagged off; the pointer slot still needs a
        // valid device address, so q_io stands in (never dereferenced).
        let args = LaunchArgs::new()
            .buf_at(q_io, q_byte_off)?
            .buf_at(k_in, k_byte_off)?
            .buf_at(v_in, v_byte_off)?
            .buf(q_norm.unwrap_or(q_io))
            .buf(k_norm.unwrap_or(q_io))
            .buf(k_cache)
            .buf(v_cache)
            .buf(positions)
            .buf(page_table)
            .buf(seq_len)
            .scalar(n_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(head_dim as i64)
            .scalar(page_size as i64)
            .scalar(if q_norm.is_some() { 1i64 } else { 0i64 })
            .scalar(if k_norm.is_some() { 1i64 } else { 0i64 })
            .scalar(eps)
            .scalar(theta_base);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Kernel-name suffix for a KV cache element type (F16 canonical, FP8
    /// E4M3 per-value scale-free quantization).
    pub(super) fn kv_suffix(kv_dtype: DType, what: &str) -> Result<&'static str> {
        match kv_dtype {
            DType::F16 => Ok("f16"),
            DType::F8E4M3 => Ok("fp8"),
            other => Err(ForgeError::Unsupported(format!(
                "{what}: no kernels for KV cache dtype {other}"
            ))),
        }
    }

    /// Scatter a prefill chunk's K/V rows ([n_tokens, n_kv_heads, head_dim])
    /// into the paged cache at positions base_pos..base_pos+n_tokens.
    /// `kv_dtype` selects the cache element type (f16 verbatim | fp8-e4m3
    /// per-value cast).
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_batch(
        &self,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        k_in: &DevBuffer,
        v_in: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        kv_dtype: DType,
        stream: &Stream,
    ) -> Result<()> {
        let suffix = Self::kv_suffix(kv_dtype, "kv_append_batch")?;
        let k = self.artifacts.get(&format!("kv_append_batch_{suffix}"))?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_kv_heads as u32, 1),
            block: ((head_dim as u32).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_cache)
            .buf(v_cache)
            .buf(k_in)
            .buf(v_in)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(head_dim as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Zapisuje K/V, odczytując pozycję bazową z bufora urządzenia.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_batch_device_pos_f16(
        &self,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        k_in: &DevBuffer,
        v_in: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: &DevBuffer,
        n_tokens: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("kv_append_batch_device_pos_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_kv_heads as u32, 1),
            block: ((head_dim as u32).clamp(32, 256), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_cache)
            .buf(v_cache)
            .buf(k_in)
            .buf(v_in)
            .buf(page_table)
            .buf(base_pos)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(head_dim as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Zapisuje K/V dla spłaszczonych segmentów sequence-major `[B,T]`.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_batch_segmented_f16(
        &self,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        k_in: &DevBuffer,
        v_in: &DevBuffer,
        page_tables: &DevBuffer,
        base_positions: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        max_pages: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        stream: &Stream,
    ) -> Result<()> {
        let (grid_x, grid_y, block) = validate_kv_append_batch_segmented_f16(
            k_cache.len(),
            v_cache.len(),
            k_in.len(),
            v_in.len(),
            page_tables.len(),
            base_positions.len(),
            batch,
            n_tokens,
            max_pages,
            n_kv_heads,
            page_size,
            head_dim,
            self.device.caps().max_threads_per_block,
        )?;
        let kernel = self.artifacts.get("kv_append_batch_segmented_f16")?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_cache)
            .buf(v_cache)
            .buf(k_in)
            .buf(v_in)
            .buf(page_tables)
            .buf(base_positions)
            .scalar(i64::try_from(n_tokens).expect("T append KV sprawdzone przez validator"))
            .scalar(
                i64::try_from(max_pages).expect("max_pages append KV sprawdzone przez validator"),
            )
            .scalar(
                i64::try_from(n_kv_heads)
                    .expect("liczba głów append KV sprawdzona przez validator"),
            )
            .scalar(
                i64::try_from(page_size)
                    .expect("rozmiar strony append KV sprawdzony przez validator"),
            )
            .scalar(
                i64::try_from(head_dim).expect("head_dim append KV sprawdzone przez validator"),
            );
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Zapisuje K/V tylko dla prefiksu zatwierdzonego decyzją każdego lane.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_append_batch_segmented_masked_f16(
        &self,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        k_in: &DevBuffer,
        v_in: &DevBuffer,
        page_tables: &DevBuffer,
        base_positions: &DevBuffer,
        decisions: &DevBuffer,
        batch: usize,
        n_tokens: usize,
        max_pages: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        stream: &Stream,
    ) -> Result<()> {
        let (grid_x, grid_y, block) = validate_kv_append_batch_segmented_masked_f16(
            k_cache.len(),
            v_cache.len(),
            k_in.len(),
            v_in.len(),
            page_tables.len(),
            base_positions.len(),
            decisions.len(),
            batch,
            n_tokens,
            max_pages,
            n_kv_heads,
            page_size,
            head_dim,
        )?;
        let kernel = self.artifacts.get("kv_append_batch_segmented_masked_f16")?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_cache)
            .buf(v_cache)
            .buf(k_in)
            .buf(v_in)
            .buf(page_tables)
            .buf(base_positions)
            .buf(decisions)
            .scalar(
                i64::try_from(n_tokens).map_err(|_| {
                    ForgeError::Kernel("T maskowanego append KV przekracza i64".into())
                })?,
            )
            .scalar(
                i64::try_from(max_pages)
                    .map_err(|_| ForgeError::Kernel("max_pages append KV przekracza i64".into()))?,
            )
            .scalar(
                i64::try_from(n_kv_heads).map_err(|_| {
                    ForgeError::Kernel("n_kv_heads append KV przekracza i64".into())
                })?,
            )
            .scalar(
                i64::try_from(page_size)
                    .map_err(|_| ForgeError::Kernel("page_size append KV przekracza i64".into()))?,
            )
            .scalar(
                i64::try_from(head_dim)
                    .map_err(|_| ForgeError::Kernel("head_dim append KV przekracza i64".into()))?,
            );
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Commit T tokens already resident in the paged f16 K/V cache
    /// (positions base_pos..base_pos+T) into the rotational low-bit store
    /// (rotquant.mojo: WHT rotate + 3/4-bit pack + per-(token,head) f16 scale).
    /// Grid (T, n_kv_heads); one thread per (token, head) vector.
    #[allow(clippy::too_many_arguments)]
    pub fn kv_pack_rot_from_cache(
        &self,
        k_packed: &DevBuffer,
        v_packed: &DevBuffer,
        k_scale: &DevBuffer,
        v_scale: &DevBuffer,
        k_cache: &DevBuffer,
        v_cache: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        bits: u8,
        stream: &Stream,
    ) -> Result<()> {
        let name = Self::rot_kernel_name("kv_pack_rot_from_cache", head_dim, bits)?;
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_kv_heads as u32, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_packed)
            .buf(v_packed)
            .buf(k_scale)
            .buf(v_scale)
            .buf(k_cache)
            .buf(v_cache)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Rotate+quant+pack a batch of T linear (rope'd) K/V rows
    /// ([n_tokens, n_kv_heads, head_dim] f16) into the paged rotational store at
    /// the absolute positions in `positions` ([T] i32, one per token), writing
    /// the rotated f16 vectors into the residual ring at `pos % ring_slots` (the
    /// recent-window fidelity copy the decode attention reads directly). Reading
    /// the position from a device buffer keeps decode launches graph-capturable.
    /// Grid (T, n_kv_heads).
    #[allow(clippy::too_many_arguments)]
    pub fn kv_pack_rot(
        &self,
        k_packed: &DevBuffer,
        v_packed: &DevBuffer,
        k_scale: &DevBuffer,
        v_scale: &DevBuffer,
        k_ring: &DevBuffer,
        v_ring: &DevBuffer,
        k_in: &DevBuffer,
        k_in_byte_off: usize,
        v_in: &DevBuffer,
        v_in_byte_off: usize,
        page_table: &DevBuffer,
        positions: &DevBuffer,
        n_tokens: usize,
        n_kv_heads: usize,
        page_size: usize,
        head_dim: usize,
        ring_slots: usize,
        bits: u8,
        stream: &Stream,
    ) -> Result<()> {
        let name = Self::rot_kernel_name("kv_pack_rot", head_dim, bits)?;
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_kv_heads as u32, 1),
            block: (1, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(k_packed)
            .buf(v_packed)
            .buf(k_scale)
            .buf(v_scale)
            .buf(k_ring)
            .buf(v_ring)
            .buf_at(k_in, k_in_byte_off)?
            .buf_at(v_in, v_in_byte_off)?
            .buf(page_table)
            .buf(positions)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(ring_slots as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Split-K rotational low-bit decode attention over the dual-region store:
    /// reads the residual f16 ring for the recent `ring_slots` positions (rotated
    /// f16, no unpack) and the packed 3/4-bit store for everything older. Rotates
    /// q once (block-cooperative WHT), scores in rotated space
    /// ((R·q)·k_rot = q·k), and writes each (seq, head, split) an UNNORMALIZED
    /// rotated partial to `parts` ([n_seqs, n_q_heads, n_splits, head_dim + 2]
    /// f32). `attn_decode_combine_rot` merges the splits and inverse-rotates.
    /// `ring_slots == 0` degrades to packed-only. Grid (n_seqs, n_q_heads,
    /// n_splits); block ATTN_BLOCK.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_rot(
        &self,
        parts: &DevBuffer,
        q: &DevBuffer,
        q_byte_off: usize,
        k_packed: &DevBuffer,
        v_packed: &DevBuffer,
        k_scale: &DevBuffer,
        v_scale: &DevBuffer,
        k_ring: &DevBuffer,
        v_ring: &DevBuffer,
        page_table: &DevBuffer,
        seq_lens: &DevBuffer,
        n_seqs: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        max_pages: usize,
        n_splits: usize,
        ring_slots: usize,
        bits: u8,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let name = Self::rot_kernel_name("attn_decode_rot", head_dim, bits)?;
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_seqs as u32, n_q_heads as u32, n_splits as u32),
            block: (ATTN_BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(parts)
            .buf_at(q, q_byte_off)?
            .buf(k_packed)
            .buf(v_packed)
            .buf(k_scale)
            .buf(v_scale)
            .buf(k_ring)
            .buf(v_ring)
            .buf(page_table)
            .buf(seq_lens)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(max_pages as i64)
            .scalar(n_splits as i64)
            .scalar(ring_slots as i64)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Merge attn_decode_rot's per-split rotated partials into the final
    /// [n_seqs, n_q_heads, head_dim] f16 output and inverse-rotate once per head
    /// (one warp per head, split order). Head_dim {64,128}.
    #[allow(clippy::too_many_arguments)]
    pub fn attn_decode_combine_rot(
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
            64 => "attn_decode_combine_rot_hd64",
            128 => "attn_decode_combine_rot_hd128",
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "attn_decode_combine_rot: head_dim {other} has no compiled specialization"
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

    /// Rotational low-bit causal prefill attention over the packed store: query
    /// token t attends positions 0..base_pos+t. Packed-only (the residual ring's
    /// recent window would be overwritten within a chunk). Grid (T, n_q_heads),
    /// one warp per (token, head).
    #[allow(clippy::too_many_arguments)]
    pub fn attn_prefill_rot(
        &self,
        out: &DevBuffer,
        q: &DevBuffer,
        k_packed: &DevBuffer,
        v_packed: &DevBuffer,
        k_scale: &DevBuffer,
        v_scale: &DevBuffer,
        page_table: &DevBuffer,
        base_pos: usize,
        n_tokens: usize,
        n_q_heads: usize,
        n_kv_heads: usize,
        head_dim: usize,
        page_size: usize,
        bits: u8,
        scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let name = Self::rot_kernel_name("attn_prefill_rot", head_dim, bits)?;
        let k = self.artifacts.get(&name)?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, n_q_heads as u32, 1),
            block: (32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(q)
            .buf(k_packed)
            .buf(v_packed)
            .buf(k_scale)
            .buf(v_scale)
            .buf(page_table)
            .scalar(base_pos as i64)
            .scalar(n_q_heads as i64)
            .scalar(n_kv_heads as i64)
            .scalar(page_size as i64)
            .scalar(scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Kernel name for a rotational specialization: `<base>_hd{64,128}_b{3,4}`.
    fn rot_kernel_name(base: &str, head_dim: usize, bits: u8) -> Result<String> {
        if bits != 3 && bits != 4 {
            return Err(ForgeError::Unsupported(format!(
                "rotational KV supports 3 or 4 bits, got {bits}"
            )));
        }
        match head_dim {
            64 | 128 => Ok(format!("{base}_hd{head_dim}_b{bits}")),
            other => Err(ForgeError::Unsupported(format!(
                "rotational KV: head_dim {other} has no compiled specialization"
            ))),
        }
    }
}
