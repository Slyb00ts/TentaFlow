// ===== File: deltanet.rs — launchery DeltaNet: splot, skan, checkpointy =====
use super::*;

impl Kernels {
    pub fn supports_deltanet_gated_scan_persistent_d128_f16(&self) -> bool {
        let caps = self.device.caps();
        caps.warp_size == 32
            && caps.max_threads_per_block >= 64
            && self
                .artifacts
                .has("deltanet_gated_scan_persistent_d128_f16")
    }

    /// Wybiera układ ValueKey tylko wtedy, gdy cały zestaw operacji jest dostępny.
    pub fn preferred_delta_state_layout(&self, d_state: usize) -> DeltaStateLayout {
        let caps = self.device.caps();
        let complete = has_delta_value_key_artifacts(|name| self.artifacts.has(name));
        delta_state_layout_dispatch(
            d_state,
            caps.warp_size,
            caps.max_threads_per_block,
            complete,
        )
    }

    pub fn supports_deltanet_prepare_tiled_d128_c4_f16(&self) -> bool {
        let caps = self.device.caps();
        caps.max_threads_per_block >= 128
            && self.artifacts.has("deltanet_prepare_tiled_d128_c4_f16")
    }

    /// Depthwise causal conv (width `d_conv`) + SiLU, one DeltaNet decode step.
    /// `win_io` [conv_dim, d_conv-1] (oldest first) is advanced in place;
    /// `weight` is ggml ssm_conv1d {d_conv, conv_dim} flattened. Grid-stride
    /// over channels.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_conv_silu_f16(
        &self,
        out: &DevBuffer,
        win_io: &DevBuffer,
        x_new: &DevBuffer,
        weight: &DevBuffer,
        conv_dim: usize,
        d_conv: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_conv_silu_f16")?;
        let cfg = LaunchConfig {
            grid: ((conv_dim as u32).div_ceil(256).min(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(win_io)
            .buf(x_new)
            .buf(weight)
            .scalar(conv_dim as i64)
            .scalar(d_conv as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Jeden krok splotu dla wiersza macierzy batcha wskazanego offsetami.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_conv_silu_f16_at(
        &self,
        out: &DevBuffer,
        out_byte_off: usize,
        win_io: &DevBuffer,
        x_new: &DevBuffer,
        x_byte_off: usize,
        weight: &DevBuffer,
        conv_dim: usize,
        d_conv: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_conv_silu_f16")?;
        let cfg = LaunchConfig {
            grid: ((conv_dim as u32).div_ceil(256).min(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(out, out_byte_off)?
            .buf(win_io)
            .buf_at(x_new, x_byte_off)?
            .buf(weight)
            .scalar(conv_dim as i64)
            .scalar(d_conv as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Czy scalony wstęp kroku DeltaNet ma artefakt i pasuje do geometrii.
    ///
    /// Podział bloków wymaga, żeby każda głowica V mapowała się na głowicę K
    /// przez modulo `n_k`, czyli `n_v` musi być wielokrotnością `n_k`, a blok
    /// ma `d_state` wątków — powyżej 1024 nie zmieściłby się w bloku.
    pub fn deltanet_step_prepare_capable(
        &self,
        d_state: usize,
        n_k_heads: usize,
        n_v_heads: usize,
    ) -> bool {
        self.artifacts.get("deltanet_step_prepare_f16").is_ok()
            && n_k_heads > 0
            && d_state > 0
            && d_state <= 1024
            && n_v_heads % n_k_heads == 0
    }

    /// Cały wstęp jednotokenowego kroku DeltaNet w jednym uruchomieniu:
    /// splot+SiLU, wycięcie v, normalizacje L2 głowic q/k, powielenie GQA,
    /// log-decay i bramka beta. Zastępuje siedem uruchomień na warstwę.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_step_prepare_f16(
        &self,
        q_dst: &DevBuffer,
        k_dst: &DevBuffer,
        v_dst: &DevBuffer,
        g_out: &DevBuffer,
        beta_out: &DevBuffer,
        win_io: &DevBuffer,
        x_new: &DevBuffer,
        x_byte_off: usize,
        conv_w: &DevBuffer,
        alpha_in: &DevBuffer,
        alpha_byte_off: usize,
        beta_in: &DevBuffer,
        beta_byte_off: usize,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        d_state: usize,
        n_k_heads: usize,
        n_v_heads: usize,
        d_conv: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if !self.deltanet_step_prepare_capable(d_state, n_k_heads, n_v_heads) {
            return Err(ForgeError::Kernel(
                "deltanet_step_prepare_f16: brak artefaktu albo geometria poza kontraktem".into(),
            ));
        }
        let rep = n_v_heads / n_k_heads;
        let value_span = checked_buffer_bytes("deltanet_step_prepare v", &[n_v_heads, d_state], 2)?;
        if q_dst.len() < value_span || k_dst.len() < value_span || v_dst.len() < value_span {
            return Err(ForgeError::Kernel(
                "deltanet_step_prepare_f16: bufor q/k/v mniejszy od kształtu głowic V".into(),
            ));
        }
        let conv_span = checked_buffer_bytes(
            "deltanet_step_prepare wejscie",
            &[n_k_heads * 2 + n_v_heads, d_state],
            2,
        )?;
        if x_byte_off.checked_add(conv_span).is_none_or(|end| end > x_new.len()) {
            return Err(ForgeError::Kernel(
                "deltanet_step_prepare_f16: okno wejścia wykracza poza bufor".into(),
            ));
        }
        let k = self.artifacts.get("deltanet_step_prepare_f16")?;
        let cfg = LaunchConfig {
            grid: (n_k_heads as u32, 1, 1),
            block: (d_state as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(q_dst)
            .buf(k_dst)
            .buf(v_dst)
            .buf(g_out)
            .buf(beta_out)
            .buf(win_io)
            .buf_at(x_new, x_byte_off)?
            .buf(conv_w)
            .buf_at(alpha_in, alpha_byte_off)?
            .buf_at(beta_in, beta_byte_off)?
            .buf(dt_bias)
            .buf(a_scale)
            .scalar(d_state as i64)
            .scalar(n_k_heads as i64)
            .scalar(rep as i64)
            .scalar(d_conv as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Powtarza bloki q/k głowic K `rep` razy do buforów głowic V (GQA).
    ///
    /// Zastępuje `2 * rep` kopii D2D JEDNYM uruchomieniem. Kopie były tanie w
    /// bajtach (1,1 us każda), ale przy 48 warstwach DeltaNet dawały 384
    /// uruchomienia na token, a każde kosztuje jeszcze ~3,8 us przestoju.
    pub fn deltanet_repeat_qk_f16(
        &self,
        q_dst: &DevBuffer,
        k_dst: &DevBuffer,
        q_src: &DevBuffer,
        k_src: &DevBuffer,
        n_elems: usize,
        rep: usize,
        stream: &Stream,
    ) -> Result<()> {
        if n_elems == 0 || rep == 0 {
            return Err(ForgeError::Kernel(
                "deltanet_repeat_qk_f16 wymaga n_elems > 0 i rep > 0".into(),
            ));
        }
        let total = n_elems.checked_mul(rep).ok_or_else(|| {
            ForgeError::Kernel("deltanet_repeat_qk_f16: przepełnienie rozmiaru".into())
        })?;
        if q_src.len() < n_elems * 2
            || k_src.len() < n_elems * 2
            || q_dst.len() < total * 2
            || k_dst.len() < total * 2
        {
            return Err(ForgeError::Kernel(
                "deltanet_repeat_qk_f16: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let k = self.artifacts.get("deltanet_repeat_qk_f16")?;
        let cfg = LaunchConfig::linear(
            u32::try_from(total).map_err(|_| {
                ForgeError::Kernel("deltanet_repeat_qk_f16: siatka przekracza u32".into())
            })?,
            BLOCK,
        );
        let args = LaunchArgs::new()
            .buf(q_dst)
            .buf(k_dst)
            .buf(q_src)
            .buf(k_src)
            .scalar(n_elems as i64)
            .scalar(rep as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// One Gated-DeltaNet recurrence step per value-head (grid = n_v_heads,
    /// block = d_state). `state_io` [n_v_heads, d_state, d_state] f32 is
    /// updated in place; q/k must already be L2-normed and repeated to
    /// n_v_heads. `g`/`beta` are the per-head log-decay / write gate (f32).
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_step_f16(
        &self,
        out: &DevBuffer,
        state_io: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        let (grid_x, block_x) = validate_deltanet_gated_step_f16(
            out.len(),
            state_io.len(),
            q.len(),
            k.len(),
            v.len(),
            g.len(),
            beta.len(),
            n_v_heads,
            d_state,
            self.device.caps().max_threads_per_block,
        )?;
        let k_art = self.artifacts.get("deltanet_gated_step_f16")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (block_x, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(state_io)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(d_state as i64);
        self.device.launch(k_art, &cfg, &args, stream)
    }

    /// Scala przygotowanie krótkiego przebiegu DeltaNet dla 2-4 tokenów.
    /// Stan okna wejściowego pozostaje niezmieniony, a checkpointy są token-major.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_prepare_f16(
        &self,
        q_out: &DevBuffer,
        k_out: &DevBuffer,
        v_out: &DevBuffer,
        g_out: &DevBuffer,
        beta_out: &DevBuffer,
        conv_checkpoints: &DevBuffer,
        conv_initial: &DevBuffer,
        qkv_mixed: &DevBuffer,
        conv_weight: &DevBuffer,
        alpha_raw: &DevBuffer,
        beta_raw: &DevBuffer,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        n_steps: usize,
        n_k_heads: usize,
        n_v_heads: usize,
        d_state: usize,
        d_conv: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        // Wariant `tokens` dzieli tokeny na drugą oś siatki. Wariant dynamiczny
        // ma JEDEN BLOK NA GŁOWĘ i przechodzi wszystkie tokeny w pętli — dla 27B
        // to 64 bloki na karcie o 64 CU, czyli dwie fale na SIMD przy 1024
        // iteracjach szeregowo. Jedyna zależność między tokenami to przyczynowy
        // splot o oknie `d_conv - 1`, którego wejście już leży w pamięci, więc
        // podział niczego nie łamie. Zmierzone na R9700 (kształt 27B, T=1024):
        // 3036 us wobec 346 us, czyli 8,8x, wynik bitowo identyczny.
        let tokens_variant = n_steps > 4 && self.artifacts.has("deltanet_prepare_tokens_f16_t32");
        let kernel_name = match n_steps {
            2 => "deltanet_prepare_t2_f16",
            3 => "deltanet_prepare_t3_f16",
            4 => "deltanet_prepare_t4_f16",
            _ if tokens_variant => "deltanet_prepare_tokens_f16_t32",
            1.. => "deltanet_prepare_dynamic_f16",
            _ => return Err(ForgeError::Kernel("deltanet_prepare wymaga T > 0".into())),
        };
        let caps = self.device.caps();
        if n_k_heads == 0
            || n_v_heads == 0
            || !n_v_heads.is_multiple_of(n_k_heads)
            || d_state == 0
            || d_state.max(32) > caps.max_threads_per_block as usize
            || d_conv < 2
            || !eps.is_finite()
            || eps < 0.0
        {
            return Err(ForgeError::Kernel(format!(
                "deltanet_prepare: niepoprawny kształt n_k={n_k_heads}, n_v={n_v_heads}, d_state={d_state}, d_conv={d_conv}, eps={eps}"
            )));
        }
        let key_heads = n_k_heads.checked_mul(2).ok_or_else(|| {
            ForgeError::Kernel("deltanet_prepare: przepełnienie liczby głów".into())
        })?;
        let conv_heads = key_heads.checked_add(n_v_heads).ok_or_else(|| {
            ForgeError::Kernel("deltanet_prepare: przepełnienie liczby głów".into())
        })?;
        let conv_dim = conv_heads
            .checked_mul(d_state)
            .ok_or_else(|| ForgeError::Kernel("deltanet_prepare: przepełnienie conv_dim".into()))?;
        let window = d_conv - 1;
        let vector_bytes = checked_buffer_bytes(
            "deltanet_prepare QKV output",
            &[n_steps, n_v_heads, d_state],
            2,
        )?;
        let gate_f32_bytes =
            checked_buffer_bytes("deltanet_prepare gates output", &[n_steps, n_v_heads], 4)?;
        let gate_f16_bytes =
            checked_buffer_bytes("deltanet_prepare gates input", &[n_steps, n_v_heads], 2)?;
        let checkpoint_bytes = checked_buffer_bytes(
            "deltanet_prepare conv checkpoints",
            &[n_steps, conv_dim, window],
            2,
        )?;
        let initial_bytes =
            checked_buffer_bytes("deltanet_prepare conv initial", &[conv_dim, window], 2)?;
        let mixed_bytes =
            checked_buffer_bytes("deltanet_prepare qkv mixed", &[n_steps, conv_dim], 2)?;
        let weight_bytes =
            checked_buffer_bytes("deltanet_prepare conv weight", &[conv_dim, d_conv], 2)?;
        let parameter_bytes = checked_buffer_bytes("deltanet_prepare parameters", &[n_v_heads], 2)?;
        if q_out.len() < vector_bytes
            || k_out.len() < vector_bytes
            || v_out.len() < vector_bytes
            || g_out.len() < gate_f32_bytes
            || beta_out.len() < gate_f32_bytes
            || conv_checkpoints.len() < checkpoint_bytes
            || conv_initial.len() < initial_bytes
            || qkv_mixed.len() < mixed_bytes
            || conv_weight.len() < weight_bytes
            || alpha_raw.len() < gate_f16_bytes
            || beta_raw.len() < gate_f16_bytes
            || dt_bias.len() < parameter_bytes
            || a_scale.len() < parameter_bytes
        {
            return Err(ForgeError::Kernel(
                "deltanet_prepare: co najmniej jeden bufor jest za mały".into(),
            ));
        }
        let grid_x = u32::try_from(n_k_heads + n_v_heads).map_err(|_| {
            ForgeError::Kernel("deltanet_prepare: liczba głów przekracza u32".into())
        })?;
        // Wariant `tokens` bierze po 32 tokeny na blok; dynamiczny ma jedną
        // płaszczyznę i przechodzi wszystkie tokeny w pętli.
        let grid_y = if tokens_variant {
            u32::try_from(n_steps.div_ceil(32)).map_err(|_| {
                ForgeError::Kernel("deltanet_prepare: liczba kawałków przekracza u32".into())
            })?
        } else {
            1
        };
        let block_x = u32::try_from(d_state.max(32)).map_err(|_| {
            ForgeError::Kernel("deltanet_prepare: rozmiar bloku przekracza u32".into())
        })?;
        let n_k_heads = i64::try_from(n_k_heads)
            .map_err(|_| ForgeError::Kernel("deltanet_prepare: n_k_heads przekracza i64".into()))?;
        let n_v_heads = i64::try_from(n_v_heads)
            .map_err(|_| ForgeError::Kernel("deltanet_prepare: n_v_heads przekracza i64".into()))?;
        let d_state = i64::try_from(d_state)
            .map_err(|_| ForgeError::Kernel("deltanet_prepare: d_state przekracza i64".into()))?;
        let d_conv = i64::try_from(d_conv)
            .map_err(|_| ForgeError::Kernel("deltanet_prepare: d_conv przekracza i64".into()))?;
        let kernel = self.artifacts.get(kernel_name)?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (block_x, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(q_out)
            .buf(k_out)
            .buf(v_out)
            .buf(g_out)
            .buf(beta_out)
            .buf(conv_checkpoints)
            .buf(conv_initial)
            .buf(qkv_mixed)
            .buf(conv_weight)
            .buf(alpha_raw)
            .buf(beta_raw)
            .buf(dt_bias)
            .buf(a_scale);
        let args = if n_steps > 4 || n_steps == 1 {
            args.scalar(n_steps as i64)
        } else {
            args
        };
        let args = args
            .scalar(n_k_heads)
            .scalar(n_v_heads)
            .scalar(d_state)
            .scalar(d_conv)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Przygotowuje niezależne segmenty DeltaNet w układzie `[B,T]`.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_prepare_segmented_f16(
        &self,
        q_out: &DevBuffer,
        k_out: &DevBuffer,
        v_out: &DevBuffer,
        g_out: &DevBuffer,
        beta_out: &DevBuffer,
        conv_checkpoints: &DevBuffer,
        conv_initial: &DevBuffer,
        qkv_mixed: &DevBuffer,
        conv_weight: &DevBuffer,
        alpha_raw: &DevBuffer,
        beta_raw: &DevBuffer,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        batch: usize,
        n_steps: usize,
        n_k_heads: usize,
        n_v_heads: usize,
        d_state: usize,
        d_conv: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || n_steps == 0 || d_state == 0 || d_state > 1024 {
            return Err(ForgeError::Kernel(
                "segmentowane przygotowanie DeltaNet wymaga B,T,d_state > 0".into(),
            ));
        }
        let kernel = self.artifacts.get("deltanet_prepare_segmented_f16")?;
        let config = LaunchConfig {
            grid: ((n_k_heads + n_v_heads) as u32, batch as u32, 1),
            block: (d_state.max(32) as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(q_out)
            .buf(k_out)
            .buf(v_out)
            .buf(g_out)
            .buf(beta_out)
            .buf(conv_checkpoints)
            .buf(conv_initial)
            .buf(qkv_mixed)
            .buf(conv_weight)
            .buf(alpha_raw)
            .buf(beta_raw)
            .buf(dt_bias)
            .buf(a_scale)
            .scalar(n_steps as i64)
            .scalar(n_k_heads as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64)
            .scalar(d_conv as i64)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Przygotowuje segmenty DeltaNet, zachowując tylko końcowe okno conv.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_prepare_segmented_final_f16(
        &self,
        q_out: &DevBuffer,
        k_out: &DevBuffer,
        v_out: &DevBuffer,
        g_out: &DevBuffer,
        beta_out: &DevBuffer,
        conv_final: &DevBuffer,
        conv_initial: &DevBuffer,
        qkv_mixed: &DevBuffer,
        conv_weight: &DevBuffer,
        alpha_raw: &DevBuffer,
        beta_raw: &DevBuffer,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        batch: usize,
        n_steps: usize,
        n_k_heads: usize,
        n_v_heads: usize,
        d_state: usize,
        d_conv: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if [batch, n_steps, n_k_heads, n_v_heads, d_state, d_conv].contains(&0)
            || d_state > 1024
            || !n_v_heads.is_multiple_of(n_k_heads)
        {
            return Err(ForgeError::Kernel(
                "segmentowane przygotowanie final wymaga poprawnych niezerowych wymiarów".into(),
            ));
        }
        let total = batch
            .checked_mul(n_steps)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie B×T DeltaNet final".into()))?;
        let conv_dim = n_k_heads
            .checked_mul(2)
            .and_then(|heads| heads.checked_add(n_v_heads))
            .and_then(|heads| heads.checked_mul(d_state))
            .ok_or_else(|| ForgeError::Kernel("przepełnienie conv_dim DeltaNet final".into()))?;
        let value_dim = n_v_heads
            .checked_mul(d_state)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie value_dim DeltaNet final".into()))?;
        let conv_elems = conv_dim
            .checked_mul(d_conv - 1)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie conv state DeltaNet final".into()))?;
        let vector_bytes = checked_buffer_bytes("DeltaNet final vectors", &[total, value_dim], 2)?;
        let gate_f32_bytes = checked_buffer_bytes("DeltaNet final gates", &[total, n_v_heads], 4)?;
        let gate_f16_bytes =
            checked_buffer_bytes("DeltaNet final raw gates", &[total, n_v_heads], 2)?;
        let conv_bytes = checked_buffer_bytes("DeltaNet final conv", &[batch, conv_elems], 2)?;
        let mixed_bytes = checked_buffer_bytes("DeltaNet final mixed", &[total, conv_dim], 2)?;
        let conv_weight_bytes =
            checked_buffer_bytes("DeltaNet final conv weight", &[conv_dim, d_conv], 2)?;
        let parameter_bytes = checked_buffer_bytes("DeltaNet final parameters", &[n_v_heads], 2)?;
        if q_out.len() < vector_bytes
            || k_out.len() < vector_bytes
            || v_out.len() < vector_bytes
            || g_out.len() < gate_f32_bytes
            || beta_out.len() < gate_f32_bytes
            || conv_final.len() < conv_bytes
            || conv_initial.len() < conv_bytes
            || qkv_mixed.len() < mixed_bytes
            || alpha_raw.len() < gate_f16_bytes
            || beta_raw.len() < gate_f16_bytes
            || conv_weight.len() < conv_weight_bytes
            || dt_bias.len() < parameter_bytes
            || a_scale.len() < parameter_bytes
        {
            return Err(ForgeError::Kernel(
                "segmentowane przygotowanie final ma za mały bufor".into(),
            ));
        }
        let grid_heads = n_k_heads
            .checked_add(n_v_heads)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie grid.x DeltaNet final".into()))?;
        let grid_x = u32::try_from(grid_heads)
            .map_err(|_| ForgeError::Kernel("DeltaNet final grid.x przekracza u32".into()))?;
        let grid_y = u32::try_from(batch)
            .map_err(|_| ForgeError::Kernel("DeltaNet final grid.y przekracza u32".into()))?;
        let block_x = u32::try_from(d_state.max(32))
            .map_err(|_| ForgeError::Kernel("DeltaNet final block przekracza u32".into()))?;
        let kernel = self.artifacts.get("deltanet_prepare_segmented_final_f16")?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (block_x, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(q_out)
            .buf(k_out)
            .buf(v_out)
            .buf(g_out)
            .buf(beta_out)
            .buf(conv_final)
            .buf(conv_initial)
            .buf(qkv_mixed)
            .buf(conv_weight)
            .buf(alpha_raw)
            .buf(beta_raw)
            .buf(dt_bias)
            .buf(a_scale)
            .scalar(n_steps as i64)
            .scalar(n_k_heads as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64)
            .scalar(d_conv as i64)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Przygotowuje pojedynczy prefiks DeltaNet D128/C4 w kaflach czasu T32.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_prepare_tiled_d128_c4_f16(
        &self,
        q_out: &DevBuffer,
        k_out: &DevBuffer,
        v_out: &DevBuffer,
        g_out: &DevBuffer,
        beta_out: &DevBuffer,
        conv_final: &DevBuffer,
        conv_initial: &DevBuffer,
        qkv_mixed: &DevBuffer,
        conv_weight: &DevBuffer,
        alpha_raw: &DevBuffer,
        beta_raw: &DevBuffer,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        n_steps: usize,
        n_k_heads: usize,
        n_v_heads: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        if [n_steps, n_k_heads, n_v_heads].contains(&0)
            || !n_v_heads.is_multiple_of(n_k_heads)
            || !eps.is_finite()
            || !self.supports_deltanet_prepare_tiled_d128_c4_f16()
        {
            return Err(ForgeError::Unsupported(
                "kafelkowane przygotowanie DeltaNet wymaga D128/C4 i poprawnych wymiarów".into(),
            ));
        }
        let conv_dim = n_k_heads
            .checked_mul(2)
            .and_then(|heads| heads.checked_add(n_v_heads))
            .and_then(|heads| heads.checked_mul(128))
            .ok_or_else(|| ForgeError::Kernel("przepełnienie conv_dim DeltaNet tiled".into()))?;
        let value_dim = n_v_heads
            .checked_mul(128)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie value_dim DeltaNet tiled".into()))?;
        let vector_bytes =
            checked_buffer_bytes("DeltaNet tiled vectors", &[n_steps, value_dim], 2)?;
        let gate_f32_bytes =
            checked_buffer_bytes("DeltaNet tiled gates", &[n_steps, n_v_heads], 4)?;
        let gate_f16_bytes =
            checked_buffer_bytes("DeltaNet tiled raw gates", &[n_steps, n_v_heads], 2)?;
        let conv_state_bytes =
            checked_buffer_bytes("DeltaNet tiled conv state", &[conv_dim, 3], 2)?;
        let mixed_bytes = checked_buffer_bytes("DeltaNet tiled mixed", &[n_steps, conv_dim], 2)?;
        let conv_weight_bytes =
            checked_buffer_bytes("DeltaNet tiled conv weight", &[conv_dim, 4], 2)?;
        let parameter_bytes = checked_buffer_bytes("DeltaNet tiled parameters", &[n_v_heads], 2)?;
        if q_out.len() < vector_bytes
            || k_out.len() < vector_bytes
            || v_out.len() < vector_bytes
            || g_out.len() < gate_f32_bytes
            || beta_out.len() < gate_f32_bytes
            || conv_final.len() < conv_state_bytes
            || conv_initial.len() < conv_state_bytes
            || qkv_mixed.len() < mixed_bytes
            || conv_weight.len() < conv_weight_bytes
            || alpha_raw.len() < gate_f16_bytes
            || beta_raw.len() < gate_f16_bytes
            || dt_bias.len() < parameter_bytes
            || a_scale.len() < parameter_bytes
        {
            return Err(ForgeError::Kernel(
                "kafelkowane przygotowanie DeltaNet ma za mały bufor".into(),
            ));
        }
        let grid_heads = n_k_heads
            .checked_add(n_v_heads)
            .ok_or_else(|| ForgeError::Kernel("DeltaNet tiled grid.x przepełniony".into()))?;
        let grid_x = u32::try_from(grid_heads)
            .map_err(|_| ForgeError::Kernel("DeltaNet tiled grid.x przekracza u32".into()))?;
        let steps = u32::try_from(n_steps)
            .map_err(|_| ForgeError::Kernel("DeltaNet tiled T przekracza u32".into()))?;
        for value in [n_steps, n_k_heads, n_v_heads] {
            i64::try_from(value)
                .map_err(|_| ForgeError::Kernel("wymiar DeltaNet tiled przekracza i64".into()))?;
        }
        let kernel = self.artifacts.get("deltanet_prepare_tiled_d128_c4_f16")?;
        let config = LaunchConfig {
            grid: (grid_x, steps.div_ceil(32), 1),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(q_out)
            .buf(k_out)
            .buf(v_out)
            .buf(g_out)
            .buf(beta_out)
            .buf(conv_final)
            .buf(conv_initial)
            .buf(qkv_mixed)
            .buf(conv_weight)
            .buf(alpha_raw)
            .buf(beta_raw)
            .buf(dt_bias)
            .buf(a_scale)
            .scalar(i64::try_from(n_steps).expect("T sprawdzone przez validator"))
            .scalar(i64::try_from(n_k_heads).expect("głowice K sprawdzone przez validator"))
            .scalar(i64::try_from(n_v_heads).expect("głowice V sprawdzone przez validator"))
            .scalar(128i64)
            .scalar(4i64)
            .scalar(eps);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Skanuje niezależne stany D128 dla segmentów `[B,T]`.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_segmented_d128_f16(
        &self,
        output: &DevBuffer,
        checkpoints: &DevBuffer,
        states: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        batch: usize,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || n_steps == 0 || d_state != 128 {
            return Err(ForgeError::Kernel(
                "segmentowany skan DeltaNet wymaga B,T > 0 i d_state=128".into(),
            ));
        }
        let tile_width = 64usize.min(self.device.caps().max_threads_per_block as usize);
        let grid_x = n_v_heads
            .checked_mul(d_state.div_ceil(tile_width))
            .ok_or_else(|| {
                ForgeError::Kernel("przepełnienie siatki segmentowanego skanu DeltaNet".into())
            })?;
        let kernel = self
            .artifacts
            .get("deltanet_gated_scan_segmented_d128_f16")?;
        let config = LaunchConfig {
            grid: (grid_x as u32, batch as u32, 1),
            block: (tile_width as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(checkpoints)
            .buf(states)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Skanuje segmenty D128, utrzymując stan warstwy w pamięci współdzielonej.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_segmented_shared_d128_f16(
        &self,
        output: &DevBuffer,
        final_states: &DevBuffer,
        states: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        batch: usize,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || n_steps == 0 || n_v_heads == 0 || d_state != 128 {
            return Err(ForgeError::Kernel(
                "współdzielony skan segmentowany wymaga B,T,H > 0 i d_state=128".into(),
            ));
        }
        let vector_bytes = checked_buffer_bytes(
            "współdzielony skan segmentowany wektory",
            &[batch, n_steps, n_v_heads, d_state],
            2,
        )?;
        let state_bytes = checked_buffer_bytes(
            "współdzielony skan segmentowany stany",
            &[batch, n_v_heads, d_state, d_state],
            4,
        )?;
        let gate_bytes = checked_buffer_bytes(
            "współdzielony skan segmentowany bramki",
            &[batch, n_steps, n_v_heads],
            4,
        )?;
        if output.len() < vector_bytes
            || final_states.len() < state_bytes
            || states.len() < state_bytes
            || q.len() < vector_bytes
            || k.len() < vector_bytes
            || v.len() < vector_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
        {
            return Err(ForgeError::Kernel(
                "współdzielony skan segmentowany ma za mały bufor".into(),
            ));
        }
        let grid_x = n_v_heads.checked_mul(2).ok_or_else(|| {
            ForgeError::Kernel("przepełnienie siatki współdzielonego skanu".into())
        })?;
        let grid_x = u32::try_from(grid_x).map_err(|_| {
            ForgeError::Kernel("siatka współdzielonego skanu przekracza u32".into())
        })?;
        let grid_y = u32::try_from(batch)
            .map_err(|_| ForgeError::Kernel("batch współdzielonego skanu przekracza u32".into()))?;
        for (name, value) in [("T", n_steps), ("H", n_v_heads), ("D", d_state)] {
            i64::try_from(value).map_err(|_| {
                ForgeError::Kernel(format!("{name} współdzielonego skanu przekracza i64"))
            })?;
        }
        let kernel = self
            .artifacts
            .get("deltanet_gated_scan_segmented_shared_d128_f16")?;
        let config = LaunchConfig {
            grid: (grid_x, grid_y, 1),
            block: (64, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(output)
            .buf(final_states)
            .buf(states)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Odtwarza wybrany prefiks segmentu bez pośrednich checkpointów w VRAM.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_commit_recompute_segmented_shared_d128_f32(
        &self,
        states: &DevBuffer,
        initial_states: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        decisions: &DevBuffer,
        batch: usize,
        max_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        if batch == 0 || max_steps == 0 || d_state != 128 {
            return Err(ForgeError::Kernel(
                "commit segmentowany wymaga B,T > 0 i d_state=128".into(),
            ));
        }
        let grid_x = n_v_heads
            .checked_mul(2)
            .ok_or_else(|| ForgeError::Kernel("przepełnienie siatki commitu DeltaNet".into()))?;
        let kernel = self
            .artifacts
            .get("deltanet_commit_recompute_segmented_shared_d128_f32")?;
        let config = LaunchConfig {
            grid: (grid_x as u32, batch as u32, 1),
            block: (64, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(states)
            .buf(initial_states)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .buf(decisions)
            .scalar(max_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Zatwierdza po jednej decyzji segmentowej dla każdego lane DeltaNet.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_commit_checkpoint_segmented_f32(
        &self,
        states: &DevBuffer,
        checkpoints: &DevBuffer,
        decisions: &DevBuffer,
        batch: usize,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        let state_elements = n_v_heads
            .checked_mul(d_state)
            .and_then(|value| value.checked_mul(d_state))
            .ok_or_else(|| ForgeError::Kernel("przepełnienie stanu DeltaNet".into()))?;
        let kernel = self
            .artifacts
            .get("deltanet_commit_checkpoint_segmented_f32")?;
        let config = LaunchConfig {
            grid: ((state_elements as u32).div_ceil(BLOCK), batch as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(states)
            .buf(checkpoints)
            .buf(decisions)
            .scalar(state_elements as i64)
            .scalar(n_steps as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Przyczynowy skan 2-4 kroków Gated-DeltaNet bez modyfikowania stanu
    /// wejściowego. Checkpointy mają układ [T, n_v_heads, d_state, d_state].
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_f16(
        &self,
        out: &DevBuffer,
        checkpoints: &DevBuffer,
        state_in: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.deltanet_gated_scan_f16_at(
            out,
            checkpoints,
            0,
            state_in,
            q,
            k,
            v,
            g,
            beta,
            n_steps,
            n_v_heads,
            d_state,
            stream,
        )
    }

    /// Przyczynowy skan Gated-DeltaNet zapisujący checkpointy od podanego
    /// przesunięcia bajtowego w większym buforze współdzielonym przez warstwy.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_f16_at(
        &self,
        out: &DevBuffer,
        checkpoints: &DevBuffer,
        checkpoint_byte_offset: usize,
        state_in: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        validate_f32_byte_offset("deltanet_gated_scan", checkpoint_byte_offset)?;
        let caps = self.device.caps();
        let dynamic_tiled =
            std::env::var("FORGE_DELTANET_SCAN_TILED").map_or(true, |value| value != "0");
        let tiled = d_state <= 128
            && (matches!(n_steps, 3 | 4) || (n_steps != 2 && dynamic_tiled))
            && caps.warp_size > 0
            && caps.warp_size <= caps.max_threads_per_block
            && caps.warp_size <= 128;
        let kernel_name = match (n_steps, tiled) {
            (2, _) => "deltanet_gated_scan_t2_f16",
            (3, true) => "deltanet_gated_scan_t3_d128_f16",
            (4, true) => "deltanet_gated_scan_t4_d128_f16",
            (3, false) => "deltanet_gated_scan_t3_f16",
            (4, false) => "deltanet_gated_scan_t4_f16",
            (1 | 5.., true) => "deltanet_gated_scan_dynamic_d128_f16",
            (1.., false) => "deltanet_gated_scan_dynamic_f16",
            _ => {
                return Err(ForgeError::Kernel(
                    "deltanet_gated_scan wymaga T > 0".into(),
                ))
            }
        };
        if n_v_heads == 0 || d_state == 0 || d_state > 1024 {
            return Err(ForgeError::Kernel(format!(
                "deltanet_gated_scan wymaga n_v_heads > 0 i 1 <= d_state <= 1024, otrzymano n_v_heads={n_v_heads}, d_state={d_state}"
            )));
        }
        let output_bytes = checked_buffer_bytes(
            "deltanet_gated_scan output",
            &[n_steps, n_v_heads, d_state],
            2,
        )?;
        let state_bytes = checked_buffer_bytes(
            "deltanet_gated_scan state",
            &[n_v_heads, d_state, d_state],
            4,
        )?;
        let checkpoint_bytes = checked_buffer_bytes(
            "deltanet_gated_scan checkpoints",
            &[n_steps, n_v_heads, d_state, d_state],
            4,
        )?;
        let gate_bytes =
            checked_buffer_bytes("deltanet_gated_scan gates", &[n_steps, n_v_heads], 4)?;
        let checkpoint_end = checkpoint_byte_offset
            .checked_add(checkpoint_bytes)
            .ok_or_else(|| {
                ForgeError::Kernel("deltanet_gated_scan: przepełnienie offsetu checkpointów".into())
            })?;
        if out.len() < output_bytes
            || checkpoints.len() < checkpoint_end
            || state_in.len() < state_bytes
            || q.len() < output_bytes
            || k.len() < output_bytes
            || v.len() < output_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
        {
            return Err(ForgeError::Kernel(
                "deltanet_gated_scan: co najmniej jeden bufor jest za mały".into(),
            ));
        }
        let block_x = if tiled {
            caps.warp_size
        } else {
            u32::try_from(d_state).map_err(|_| {
                ForgeError::Kernel("deltanet_gated_scan: d_state przekracza u32".into())
            })?
        };
        let head_tiles = if tiled {
            d_state.div_ceil(block_x as usize)
        } else {
            1
        };
        let grid_heads = n_v_heads.checked_mul(head_tiles).ok_or_else(|| {
            ForgeError::Kernel("deltanet_gated_scan: przepełnienie liczby kafli".into())
        })?;
        let grid_x = u32::try_from(grid_heads).map_err(|_| {
            ForgeError::Kernel("deltanet_gated_scan: liczba głów przekracza u32".into())
        })?;
        let k_art = self.artifacts.get(kernel_name)?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (block_x, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(checkpoints, checkpoint_byte_offset)?
            .buf(state_in)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta);
        let args = if n_steps > 4 || n_steps == 1 {
            args.scalar(n_steps as i64)
        } else {
            args
        };
        let args = args.scalar(n_v_heads as i64).scalar(d_state as i64);
        self.device.launch(k_art, &cfg, &args, stream)
    }

    /// Wykonuje dynamiczny skan prefill bezpośrednio na stanie końcowym.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_inplace_f16(
        &self,
        out: &DevBuffer,
        state_io: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.deltanet_gated_scan_inplace_f16_at(
            out, state_io, q, k, v, g, beta, 0, n_steps, n_v_heads, d_state, stream,
        )
    }

    /// Wykonuje fragment skanu na wierszach większych macierzy token-major.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_inplace_f16_at(
        &self,
        out: &DevBuffer,
        state_io: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        token_offset: usize,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        let caps = self.device.caps();
        if n_steps == 0
            || n_v_heads == 0
            || d_state == 0
            || d_state > 128
            || caps.warp_size == 0
            || caps.warp_size > 128
            || caps.warp_size > caps.max_threads_per_block
        {
            return Err(ForgeError::Kernel(format!(
                "in-place DeltaNet wymaga T>0, heads>0, d_state<=128 i poprawnego warp, otrzymano T={n_steps}, heads={n_v_heads}, d_state={d_state}"
            )));
        }
        let vector_bytes = checked_buffer_bytes(
            "in-place DeltaNet vectors",
            &[n_steps, n_v_heads, d_state],
            2,
        )?;
        let state_bytes =
            checked_buffer_bytes("in-place DeltaNet state", &[n_v_heads, d_state, d_state], 4)?;
        let gate_bytes = checked_buffer_bytes("in-place DeltaNet gates", &[n_steps, n_v_heads], 4)?;
        let vector_byte_offset = checked_buffer_bytes(
            "in-place DeltaNet vector offset",
            &[token_offset, n_v_heads, d_state],
            2,
        )?;
        let gate_byte_offset = checked_buffer_bytes(
            "in-place DeltaNet gate offset",
            &[token_offset, n_v_heads],
            4,
        )?;
        let vector_end = vector_byte_offset
            .checked_add(vector_bytes)
            .ok_or_else(|| {
                ForgeError::Kernel("in-place DeltaNet: przepełnienie zakresu wektorów".into())
            })?;
        let gate_end = gate_byte_offset.checked_add(gate_bytes).ok_or_else(|| {
            ForgeError::Kernel("in-place DeltaNet: przepełnienie zakresu bramek".into())
        })?;
        if out.len() < vector_end
            || state_io.len() < state_bytes
            || q.len() < vector_end
            || k.len() < vector_end
            || v.len() < vector_end
            || g.len() < gate_end
            || beta.len() < gate_end
        {
            return Err(ForgeError::Kernel(
                "in-place DeltaNet: co najmniej jeden bufor jest za mały".into(),
            ));
        }
        let tile_width = (caps.warp_size as usize)
            .max(64)
            .min(d_state)
            .min(caps.max_threads_per_block as usize);
        let tiles = d_state.div_ceil(tile_width);
        let grid = n_v_heads
            .checked_mul(tiles)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| ForgeError::Kernel("in-place DeltaNet: grid przekracza u32".into()))?;
        let kernel_name = if n_steps == 128 && d_state == 128 && tile_width == 64 {
            "deltanet_gated_scan_inplace_shared_d128_f16"
        } else {
            "deltanet_gated_scan_inplace_dynamic_d128_f16"
        };
        let kernel = self.artifacts.get(kernel_name)?;
        let config = LaunchConfig {
            grid: (grid, 1, 1),
            block: (tile_width as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(out, vector_byte_offset)?
            .buf(state_io)
            .buf_at(q, vector_byte_offset)?
            .buf_at(k, vector_byte_offset)?
            .buf_at(v, vector_byte_offset)?
            .buf_at(g, gate_byte_offset)?
            .buf_at(beta, gate_byte_offset)?
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Skanuje stan ValueKey `[B,H,value,key]`, zapisując wyłącznie stan końcowy.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_value_key_scan_inplace_f16(
        &self,
        out: &DevBuffer,
        state_out: &DevBuffer,
        state_in: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_sequences: usize,
        n_steps: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let d_state = 128usize;
        if self.preferred_delta_state_layout(d_state) != DeltaStateLayout::ValueKey
            || n_sequences == 0
            || n_steps == 0
            || n_v_heads == 0
        {
            return Err(ForgeError::Unsupported(
                "skan ValueKey wymaga kompletnego backendu d128 i niezerowych wymiarów".into(),
            ));
        }
        let vector_bytes = checked_buffer_bytes(
            "ValueKey vectors",
            &[n_sequences, n_steps, n_v_heads, d_state],
            2,
        )?;
        let state_bytes = checked_buffer_bytes(
            "ValueKey state",
            &[n_sequences, n_v_heads, d_state, d_state],
            4,
        )?;
        let gate_bytes =
            checked_buffer_bytes("ValueKey gates", &[n_sequences, n_steps, n_v_heads], 4)?;
        if out.len() < vector_bytes
            || state_out.len() < state_bytes
            || state_in.len() < state_bytes
            || q.len() < vector_bytes
            || k.len() < vector_bytes
            || v.len() < vector_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
        {
            return Err(ForgeError::Kernel(
                "skan ValueKey otrzymał za mały bufor".into(),
            ));
        }
        let caps = self.device.caps();
        let block = caps.warp_size * 4;
        let grid = n_v_heads
            .checked_mul(32)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| ForgeError::Kernel("siatka ValueKey przekracza u32".into()))?;
        let sequences = u32::try_from(n_sequences)
            .map_err(|_| ForgeError::Kernel("batch ValueKey przekracza u32".into()))?;
        let kernel = self.artifacts.get("deltanet_value_key_scan_inplace_f16")?;
        let config = LaunchConfig {
            grid: (grid, sequences, 1),
            block: (block, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(state_out)
            .buf(state_in)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(n_sequences as i64)
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Skanuje długi prefill ValueKey z dwiema kolumnami przypisanymi do warpa.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_value_key_scan_persistent_f16(
        &self,
        out: &DevBuffer,
        state_io: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let d_state = 128usize;
        if self.preferred_delta_state_layout(d_state) != DeltaStateLayout::ValueKey
            || n_steps == 0
            || n_v_heads == 0
        {
            return Err(ForgeError::Unsupported(
                "persistent ValueKey wymaga kompletnego backendu d128".into(),
            ));
        }
        let vector_bytes =
            checked_buffer_bytes("persistent ValueKey vectors", &[n_steps, n_v_heads, 128], 2)?;
        let state_bytes =
            checked_buffer_bytes("persistent ValueKey state", &[n_v_heads, 128, 128], 4)?;
        let gate_bytes =
            checked_buffer_bytes("persistent ValueKey gates", &[n_steps, n_v_heads], 4)?;
        if out.len() < vector_bytes
            || state_io.len() < state_bytes
            || q.len() < vector_bytes
            || k.len() < vector_bytes
            || v.len() < vector_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
        {
            return Err(ForgeError::Kernel(
                "persistent ValueKey otrzymał za mały bufor".into(),
            ));
        }
        let caps = self.device.caps();
        let grid = n_v_heads
            .checked_mul(32)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                ForgeError::Kernel("siatka persistent ValueKey przekracza u32".into())
            })?;
        let kernel = self
            .artifacts
            .get("deltanet_value_key_scan_persistent_f16")?;
        let config = LaunchConfig {
            grid: (grid, 1, 1),
            block: (caps.warp_size * 2, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(state_io)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Skanuje stan ValueKey i zapisuje checkpoint po każdym tokenie.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_value_key_scan_checkpoints_f16(
        &self,
        out: &DevBuffer,
        checkpoints: &DevBuffer,
        state_in: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_sequences: usize,
        n_steps: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.deltanet_value_key_scan_checkpoints_f16_at(
            out,
            checkpoints,
            0,
            state_in,
            q,
            k,
            v,
            g,
            beta,
            n_sequences,
            n_steps,
            n_v_heads,
            stream,
        )
    }

    /// Zapisuje checkpointy ValueKey od przesunięcia w większym workspace.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_value_key_scan_checkpoints_f16_at(
        &self,
        out: &DevBuffer,
        checkpoints: &DevBuffer,
        checkpoint_byte_offset: usize,
        state_in: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_sequences: usize,
        n_steps: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        validate_f32_byte_offset("checkpointy ValueKey", checkpoint_byte_offset)?;
        let d_state = 128usize;
        if self.preferred_delta_state_layout(d_state) != DeltaStateLayout::ValueKey
            || n_sequences == 0
            || n_steps == 0
            || n_v_heads == 0
        {
            return Err(ForgeError::Unsupported(
                "checkpointy ValueKey wymagają kompletnego backendu d128".into(),
            ));
        }
        let state_bytes = checked_buffer_bytes(
            "ValueKey checkpoint state",
            &[n_sequences, n_v_heads, d_state, d_state],
            4,
        )?;
        let checkpoint_bytes = state_bytes
            .checked_mul(n_steps)
            .ok_or_else(|| ForgeError::Kernel("checkpointy ValueKey przekraczają usize".into()))?;
        let vector_bytes = checked_buffer_bytes(
            "ValueKey checkpoint vectors",
            &[n_sequences, n_steps, n_v_heads, d_state],
            2,
        )?;
        let gate_bytes = checked_buffer_bytes(
            "ValueKey checkpoint gates",
            &[n_sequences, n_steps, n_v_heads],
            4,
        )?;
        let checkpoint_end = checkpoint_byte_offset
            .checked_add(checkpoint_bytes)
            .ok_or_else(|| {
                ForgeError::Kernel("offset checkpointów ValueKey przepełnia usize".into())
            })?;
        if out.len() < vector_bytes
            || checkpoints.len() < checkpoint_end
            || state_in.len() < state_bytes
            || q.len() < vector_bytes
            || k.len() < vector_bytes
            || v.len() < vector_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
        {
            return Err(ForgeError::Kernel(
                "skan checkpointów ValueKey otrzymał za mały bufor".into(),
            ));
        }
        let caps = self.device.caps();
        let grid = n_v_heads
            .checked_mul(32)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| {
                ForgeError::Kernel("siatka checkpointów ValueKey przekracza u32".into())
            })?;
        let sequences = u32::try_from(n_sequences)
            .map_err(|_| ForgeError::Kernel("batch checkpointów ValueKey przekracza u32".into()))?;
        let kernel = self
            .artifacts
            .get("deltanet_value_key_scan_checkpoints_f16")?;
        let config = LaunchConfig {
            grid: (grid, sequences, 1),
            block: (caps.warp_size * 4, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(checkpoints, checkpoint_byte_offset)?
            .buf(state_in)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(n_sequences as i64)
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Odtwarza na ValueKey zaakceptowany prefiks każdej sekwencji.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_value_key_commit_recompute_f32(
        &self,
        state_out: &DevBuffer,
        state_in: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        decisions: &DevBuffer,
        n_sequences: usize,
        max_steps: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let d_state = 128usize;
        if self.preferred_delta_state_layout(d_state) != DeltaStateLayout::ValueKey
            || n_sequences == 0
            || max_steps == 0
            || n_v_heads == 0
        {
            return Err(ForgeError::Unsupported(
                "recompute ValueKey wymaga kompletnego backendu d128".into(),
            ));
        }
        let state_bytes = checked_buffer_bytes(
            "ValueKey recompute state",
            &[n_sequences, n_v_heads, 128, 128],
            4,
        )?;
        let vector_bytes = checked_buffer_bytes(
            "ValueKey recompute vectors",
            &[n_sequences, max_steps, n_v_heads, 128],
            2,
        )?;
        let gate_bytes = checked_buffer_bytes(
            "ValueKey recompute gates",
            &[n_sequences, max_steps, n_v_heads],
            4,
        )?;
        let decision_bytes =
            checked_buffer_bytes("ValueKey recompute decisions", &[n_sequences, 2], 4)?;
        if state_out.len() < state_bytes
            || state_in.len() < state_bytes
            || k.len() < vector_bytes
            || v.len() < vector_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
            || decisions.len() < decision_bytes
        {
            return Err(ForgeError::Kernel(
                "recompute ValueKey otrzymał za mały bufor".into(),
            ));
        }
        let caps = self.device.caps();
        let grid = n_v_heads
            .checked_mul(32)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| ForgeError::Kernel("siatka recompute ValueKey przekracza u32".into()))?;
        let sequences = u32::try_from(n_sequences)
            .map_err(|_| ForgeError::Kernel("batch recompute ValueKey przekracza u32".into()))?;
        let kernel = self
            .artifacts
            .get("deltanet_value_key_commit_recompute_f32")?;
        let config = LaunchConfig {
            grid: (grid, sequences, 1),
            block: (caps.warp_size * 4, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(state_out)
            .buf(state_in)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .buf(decisions)
            .scalar(n_sequences as i64)
            .scalar(max_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(d_state as i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Wykonuje pełny rejestrowy skan DeltaNet d128 jednym uruchomieniem.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_scan_persistent_d128_f16(
        &self,
        out: &DevBuffer,
        state_io: &DevBuffer,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        g: &DevBuffer,
        beta: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        if n_steps == 0
            || n_v_heads == 0
            || !self.supports_deltanet_gated_scan_persistent_d128_f16()
        {
            return Err(ForgeError::Unsupported(
                "persistent DeltaNet wymaga T>0, heads>0 oraz NVIDIA warp32".into(),
            ));
        }
        let vector_bytes =
            checked_buffer_bytes("persistent DeltaNet vectors", &[n_steps, n_v_heads, 128], 2)?;
        let state_bytes =
            checked_buffer_bytes("persistent DeltaNet state", &[n_v_heads, 128, 128], 4)?;
        let gate_bytes =
            checked_buffer_bytes("persistent DeltaNet gates", &[n_steps, n_v_heads], 4)?;
        if out.len() < vector_bytes
            || state_io.len() < state_bytes
            || q.len() < vector_bytes
            || k.len() < vector_bytes
            || v.len() < vector_bytes
            || g.len() < gate_bytes
            || beta.len() < gate_bytes
        {
            return Err(ForgeError::Kernel(
                "persistent DeltaNet: co najmniej jeden bufor jest za mały".into(),
            ));
        }
        let grid = n_v_heads
            .checked_mul(32)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| ForgeError::Kernel("persistent DeltaNet: grid przekracza u32".into()))?;
        let kernel = self
            .artifacts
            .get("deltanet_gated_scan_persistent_d128_f16")?;
        let config = LaunchConfig {
            grid: (grid, 1, 1),
            block: (64, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(state_io)
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(g)
            .buf(beta)
            .scalar(n_steps as i64)
            .scalar(n_v_heads as i64)
            .scalar(128i64);
        self.device.launch(kernel, &config, &args, stream)
    }

    /// Zatwierdza na GPU checkpoint wskazany przez urządzeniowy licznik i32.
    /// Wartość 0 pozostawia stan bez zmian, a wartości spoza [0, T] są ignorowane.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_commit_checkpoint_f32(
        &self,
        state_out: &DevBuffer,
        checkpoints: &DevBuffer,
        accepted_index: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        self.deltanet_commit_checkpoint_f32_at(
            state_out,
            checkpoints,
            0,
            accepted_index,
            n_steps,
            n_v_heads,
            d_state,
            stream,
        )
    }

    /// Zatwierdza checkpoint z fragmentu większego bufora zaczynającego się
    /// pod podanym przesunięciem bajtowym.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_commit_checkpoint_f32_at(
        &self,
        state_out: &DevBuffer,
        checkpoints: &DevBuffer,
        checkpoint_byte_offset: usize,
        accepted_index: &DevBuffer,
        n_steps: usize,
        n_v_heads: usize,
        d_state: usize,
        stream: &Stream,
    ) -> Result<()> {
        validate_f32_byte_offset("deltanet_commit_checkpoint", checkpoint_byte_offset)?;
        if n_steps == 0 {
            return Err(ForgeError::Kernel(
                "deltanet_commit_checkpoint wymaga T > 0".into(),
            ));
        }
        if n_v_heads == 0 || d_state == 0 || d_state > 1024 {
            return Err(ForgeError::Kernel(format!(
                "deltanet_commit_checkpoint: niepoprawny kształt n_v_heads={n_v_heads}, d_state={d_state}"
            )));
        }
        let state_elements = n_v_heads
            .checked_mul(d_state)
            .and_then(|elements| elements.checked_mul(d_state))
            .ok_or_else(|| {
                ForgeError::Kernel("deltanet_commit_checkpoint: przepełnienie stanu".into())
            })?;
        let state_bytes = state_elements.checked_mul(4).ok_or_else(|| {
            ForgeError::Kernel("deltanet_commit_checkpoint: przepełnienie bajtów stanu".into())
        })?;
        let checkpoint_bytes = state_bytes.checked_mul(n_steps).ok_or_else(|| {
            ForgeError::Kernel("deltanet_commit_checkpoint: przepełnienie checkpointów".into())
        })?;
        let state_elements_i64 = i64::try_from(state_elements).map_err(|_| {
            ForgeError::Kernel("deltanet_commit_checkpoint: liczba elementów przekracza i64".into())
        })?;
        let checkpoint_end = checkpoint_byte_offset
            .checked_add(checkpoint_bytes)
            .ok_or_else(|| {
                ForgeError::Kernel(
                    "deltanet_commit_checkpoint: przepełnienie offsetu checkpointów".into(),
                )
            })?;
        if state_out.len() < state_bytes
            || checkpoints.len() < checkpoint_end
            || accepted_index.len() < std::mem::size_of::<i32>()
        {
            return Err(ForgeError::Kernel(
                "deltanet_commit_checkpoint: co najmniej jeden bufor jest za mały".into(),
            ));
        }
        let grid_x =
            u32::try_from(state_elements.div_ceil(BLOCK as usize).min(65_535)).map_err(|_| {
                ForgeError::Kernel("deltanet_commit_checkpoint: siatka przekracza u32".into())
            })?;
        let k_art = self.artifacts.get("deltanet_commit_checkpoint_f32")?;
        let cfg = LaunchConfig {
            grid: (grid_x, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(state_out)
            .buf_at(checkpoints, checkpoint_byte_offset)?
            .buf(accepted_index)
            .scalar(state_elements_i64)
            .scalar(n_steps as i64);
        self.device.launch(k_art, &cfg, &args, stream)
    }

    /// Output gated RMSNorm per value-head: out = rmsnorm(o, weight)·silu(z).
    /// One block per head, block covers `d_state`.
    #[allow(clippy::too_many_arguments)]
    /// `deltanet_gated_rmsnorm_f16` czytający bramkę `z` z przesunięcia lane'a.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_gated_rmsnorm_f16_at(
        &self,
        out: &DevBuffer,
        out_byte_off: usize,
        o_in: &DevBuffer,
        z_in: &DevBuffer,
        z_byte_off: usize,
        weight: &DevBuffer,
        n_v_heads: usize,
        d_state: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_gated_rmsnorm_f16")?;
        let cfg = LaunchConfig {
            grid: (n_v_heads as u32, 1, 1),
            block: ((d_state as u32).clamp(32, 1024), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(out, out_byte_off)?
            .buf(o_in)
            .buf_at(z_in, z_byte_off)?
            .buf(weight)
            .scalar(d_state as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    pub fn deltanet_gated_rmsnorm_f16(
        &self,
        out: &DevBuffer,
        o_in: &DevBuffer,
        z_in: &DevBuffer,
        weight: &DevBuffer,
        n_v_heads: usize,
        d_state: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.deltanet_gated_rmsnorm_f16_at(
            out, 0, o_in, z_in, 0, weight, n_v_heads, d_state, eps, stream,
        )
    }

    /// Per-head DeltaNet log-decay g = softplus(alpha + dt_bias)·a (f32 out).
    pub fn deltanet_log_decay_f32(
        &self,
        g_out: &DevBuffer,
        alpha_in: &DevBuffer,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_log_decay_f32")?;
        let cfg = LaunchConfig {
            grid: ((n_v_heads as u32).div_ceil(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(g_out)
            .buf(alpha_in)
            .buf(dt_bias)
            .buf(a_scale)
            .scalar(n_v_heads as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Wariant batchowy pojedynczego wiersza z przesunięciem buforów wejścia
    /// i wyjścia; wektory parametrów warstwy zawsze zaczynają się od zera.
    #[allow(clippy::too_many_arguments)]
    pub fn deltanet_log_decay_f32_at(
        &self,
        g_out: &DevBuffer,
        g_byte_off: usize,
        alpha_in: &DevBuffer,
        alpha_byte_off: usize,
        dt_bias: &DevBuffer,
        a_scale: &DevBuffer,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_log_decay_f32")?;
        let cfg = LaunchConfig {
            grid: ((n_v_heads as u32).div_ceil(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(g_out, g_byte_off)?
            .buf_at(alpha_in, alpha_byte_off)?
            .buf(dt_bias)
            .buf(a_scale)
            .scalar(n_v_heads as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Per-head DeltaNet write gate beta = sigmoid(beta_proj) (f32 out).
    /// `deltanet_beta_sigmoid_f32` czytający wiersz `beta_byte_off` wejścia.
    /// Pozwala batchowemu decode wziąć swój lane wprost z projekcji policzonej
    /// dla całego batcha, bez kopii do jednotokenowego scratchu.
    pub fn deltanet_beta_sigmoid_f32_at(
        &self,
        beta_out: &DevBuffer,
        beta_in: &DevBuffer,
        beta_byte_off: usize,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_beta_sigmoid_f32")?;
        let cfg = LaunchConfig {
            grid: ((n_v_heads as u32).div_ceil(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(beta_out)
            .buf_at(beta_in, beta_byte_off)?
            .scalar(n_v_heads as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    pub fn deltanet_beta_sigmoid_f32(
        &self,
        beta_out: &DevBuffer,
        beta_in: &DevBuffer,
        n_v_heads: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deltanet_beta_sigmoid_f32")?;
        let cfg = LaunchConfig {
            grid: ((n_v_heads as u32).div_ceil(256), 1, 1),
            block: (256, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(beta_out)
            .buf(beta_in)
            .scalar(n_v_heads as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// 1-D conv (kernel 3, pad 1) with fused optional GELU.
    /// x: [in_ch, in_t]; weight: [out_ch, in_ch, 3]; out: [out_ch, out_t].
    #[allow(clippy::too_many_arguments)]
    pub fn conv1d_k3_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        bias: &DevBuffer,
        in_ch: usize,
        out_ch: usize,
        in_t: usize,
        out_t: usize,
        stride: usize,
        apply_gelu: bool,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("conv1d_k3_f16")?;
        let cfg = LaunchConfig {
            grid: ((out_t as u32).div_ceil(128), out_ch as u32, 1),
            block: (128, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(weight)
            .buf(bias)
            .scalar(in_ch as i64)
            .scalar(in_t as i64)
            .scalar(out_t as i64)
            .scalar(stride as i64)
            .scalar(if apply_gelu { 1i64 } else { 0i64 });
        self.device.launch(k, &cfg, &args, stream)
    }

}
