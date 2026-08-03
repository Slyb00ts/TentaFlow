// ===== File: norm.rs — launchery normalizacji: RMSNorm, LayerNorm, L2 =====
use super::*;

/// Szerokość bloku normy wiersza. Krok dekodowania normalizuje JEDEN wiersz, więc
/// siatka ma jeden blok roboczy — cała norma liczy się na jednym CU z 64 i stoi
/// na paśmie tego CU, nie karty. Szerszy blok jest jedynym sposobem dołożenia
/// równoległości bez rozbijania redukcji na dwa uruchomienia.
fn norm_block(rows: usize, cols: usize) -> u32 {
    // Wyłącznie dekodowanie JEDNEGO wiersza. Szerszy blok zmienia kształt
    // redukcji, więc każdy inny `rows` (weryfikacja MTP T>1, prefill, batch)
    // musi zostać na dotychczasowym bloku — inaczej ta sama sekwencja liczy
    // ostatnie bity inaczej niż ścieżka jednotokenowa.
    if rows != 1 {
        return BLOCK;
    }
    let want = (cols.div_ceil(8)).next_power_of_two().clamp(64, 1024);
    u32::try_from(want).unwrap_or(BLOCK)
}

impl Kernels {
    /// out[row] = rmsnorm(x[row]) * weight, f16, one block per row.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `rmsnorm_f16` over a section of a fused buffer, addressed by byte offset
    /// (in/out share the slice). Used by the rot decode path to normalize the
    /// q/k slices of a fused qkv buffer in place.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_f16_at(
        &self,
        io: &DevBuffer,
        byte_off: usize,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf_at(io, byte_off)?
            .buf_at(io, byte_off)?
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// residual += x; out = rmsnorm(residual) * weight (fused, f16).
    #[allow(clippy::too_many_arguments)]
    /// Norma delty + rezyduum + skala warstwy + norma wyjściowa w jednym
    /// uruchomieniu (rodzina Gemma). Rozbite na trzy kernele kosztowały 144
    /// uruchomienia na token przy 48 warstwach, a każdy z nich czyta zaledwie
    /// kilka kB — to sam narzut wywołania. `layer_scale` równe 1.0 wyłącza
    /// skalowanie.
    #[allow(clippy::too_many_arguments)]
    /// Normy Q, K i V jednym uruchomieniem (rodzina Gemma normalizuje wszystkie
    /// trzy, V wektorem jedynek). Osobno kosztowały 144 wywołania na token przy
    /// 48 warstwach, a każde czyta ledwie kilka kB.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_qkv_f16(
        &self,
        q: &DevBuffer,
        k: &DevBuffer,
        v: &DevBuffer,
        wq: &DevBuffer,
        wk: &DevBuffer,
        wv: &DevBuffer,
        q_rows: usize,
        kv_rows: usize,
        head_dim: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k_fn = self.artifacts.get("rmsnorm_qkv_f16")?;
        let cfg = LaunchConfig {
            grid: ((q_rows + 2 * kv_rows) as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(q)
            .buf(k)
            .buf(v)
            .buf(wq)
            .buf(wk)
            .buf(wv)
            .scalar(q_rows as i64)
            .scalar(kv_rows as i64)
            .scalar(head_dim as i64)
            .scalar(eps);
        self.device.launch(k_fn, &cfg, &args, stream)
    }

    pub fn rmsnorm_delta_residual_f16(
        &self,
        out: &DevBuffer,
        residual_io: &DevBuffer,
        delta: &DevBuffer,
        delta_weight: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        layer_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_delta_residual_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(residual_io)
            .buf(delta)
            .buf(delta_weight)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps)
            .scalar(layer_scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    pub fn rmsnorm_residual_f16(
        &self,
        out: &DevBuffer,
        residual_io: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_residual_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (norm_block(rows, cols), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(residual_io)
            .buf(x)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Per-head L2 normalization (out = x / sqrt(Σx² + eps)); one block per
    /// head, block covers `d_state`. Used on the DeltaNet conv q/k heads.
    pub fn l2norm_heads_f16(
        &self,
        out: &DevBuffer,
        x_in: &DevBuffer,
        n_heads: usize,
        d_state: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.l2norm_heads_f16_at(out, x_in, 0, n_heads, d_state, eps, stream)
    }

    /// Ten sam kernel, ale czyta wejście od przesunięcia bajtowego.
    ///
    /// Mikser DeltaNet ciął wyjście splotu na wycinki q/k/v trzema kopiami D2D na
    /// warstwę, tylko po to, żeby konsument dostał bufor od zera. Kernel bierze
    /// wskaźnik, więc wystarczy go przesunąć — kopie były 144 uruchomieniami na
    /// token przy 48 warstwach.
    #[allow(clippy::too_many_arguments)]
    pub fn l2norm_heads_f16_at(
        &self,
        out: &DevBuffer,
        x_in: &DevBuffer,
        x_byte_off: usize,
        n_heads: usize,
        d_state: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let span = checked_buffer_bytes("l2norm_heads wejscie", &[n_heads, d_state], 2)?;
        let end = x_byte_off.checked_add(span).ok_or_else(|| {
            ForgeError::Kernel("l2norm_heads_f16_at: przepełnienie przesunięcia".into())
        })?;
        if end > x_in.len() || out.len() < span {
            return Err(ForgeError::Kernel(
                "l2norm_heads_f16_at: okno wykracza poza bufor".into(),
            ));
        }
        let k = self.artifacts.get("l2norm_heads_f16")?;
        let cfg = LaunchConfig {
            grid: (n_heads as u32, 1, 1),
            block: ((d_state as u32).clamp(32, 1024), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(x_in, x_byte_off)?
            .scalar(d_state as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Normalizacja RMS osobno dla każdej głowicy, BEZ wagi — druga norma Q
    /// rodziny DeepSeek V4, nakładana już po projekcji `wq_b`.
    pub fn rmsnorm_head_f16(
        &self,
        buf: &DevBuffer,
        head_dim: usize,
        n_heads: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_head_f16")?;
        // Redukcja idzie przez pamięć współdzieloną o stałym rozmiarze 1024,
        // więc liczba wątków musi być potęgą dwójki w tym zakresie.
        let threads = head_dim.next_power_of_two().clamp(64, 1024) as u32;
        let cfg = LaunchConfig {
            grid: (n_heads as u32, 1, 1),
            block: (threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(buf)
            .scalar(head_dim as i64)
            .scalar(n_heads as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `mixes = (mix_fn @ x) * rsqrt(mean(x^2))` — wejście Sinkhorna
    /// hyper-connections. Normalizacja obejmuje złączone kopie strumienia.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_mix_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        mix_fn: &DevBuffer,
        width: usize,
        mix_hc: usize,
        n_tokens: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_mix_f32")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(mix_fn)
            .scalar(width as i64)
            .scalar(mix_hc as i64)
            .scalar(n_tokens as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out[row] = layernorm(x[row]) * weight + bias.
    #[allow(clippy::too_many_arguments)]
    pub fn layernorm_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        bias: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("layernorm_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(weight)
            .buf(bias)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// residual += x; out = layernorm(residual) * weight + bias (fused).
    #[allow(clippy::too_many_arguments)]
    pub fn layernorm_residual_f16(
        &self,
        out: &DevBuffer,
        residual_io: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        bias: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("layernorm_residual_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(residual_io)
            .buf(x)
            .buf(weight)
            .buf(bias)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused RMSNorm → shared fp8 activation: writes the f16 normed row to
    /// `out_f16` AND the per-token e4m3 codes + f32 scale into the shared fp8
    /// activation scratch, so the following q/k/v (or gate/up) projections read
    /// ONE quantized activation via `gemm_fp8_modular_prequant` instead of
    /// re-quantizing per projection. The fp8mod analog of a fused norm→quant for the fp8mod
    /// path. `cols` is the hidden size (the projection K).
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_fp8_shared(
        &self,
        out_f16: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.fp8_act_ensure(rows * cols, rows)?;
        let sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xs = sc.xs.as_ref().expect("xs allocated");
        let k = self.artifacts.get("rmsnorm_fp8")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out_f16)
            .buf(xq)
            .buf(xs)
            .buf(x)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fused residual-add + RMSNorm → shared fp8 activation: `residual_io += x`,
    /// normed row to `out_f16`, shared per-token e4m3 codes + scale to scratch.
    /// See `rmsnorm_fp8_shared`.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_residual_fp8_shared(
        &self,
        out_f16: &DevBuffer,
        residual_io: &DevBuffer,
        x: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        self.fp8_act_ensure(rows * cols, rows)?;
        let sc = self.fp8_act.lock().expect("fp8 act scratch poisoned");
        let xq = sc.xq.as_ref().expect("xq allocated");
        let xs = sc.xs.as_ref().expect("xs allocated");
        let k = self.artifacts.get("rmsnorm_residual_fp8")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out_f16)
            .buf(xq)
            .buf(xs)
            .buf(residual_io)
            .buf(x)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Final decode norm from the (h f16, h32 f32) residual pair: out =
    /// rmsnorm(h) * weight with the sum-of-squares taken from h32.
    #[allow(clippy::too_many_arguments)]
    pub fn rmsnorm_h32_f16(
        &self,
        out: &DevBuffer,
        h: &DevBuffer,
        h32: &DevBuffer,
        weight: &DevBuffer,
        rows: usize,
        cols: usize,
        eps: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("rmsnorm_h32_f16")?;
        let cfg = LaunchConfig {
            grid: (rows as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(h)
            .buf(h32)
            .buf(weight)
            .scalar(cols as i64)
            .scalar(eps);
        self.device.launch(k, &cfg, &args, stream)
    }

}
