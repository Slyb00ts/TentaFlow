// ===== File: elementwise.rs — launchery elementowe i aktywacje =====
use super::*;

impl Kernels {
    /// out = act(gate) * up nad n elementami f16 (bramkowany FFN).
    ///
    /// Nieliniowość jest parametrem, bo rodziny modeli różnią się nią przy
    /// identycznym kształcie warstwy: SwiGLU (`silu`) w llamie i qwenie, GeGLU
    /// z przybliżeniem tanh w rodzinie Gemma.
    pub fn glu_mul_f16(
        &self,
        act: forge_formats::FfnActivation,
        out: &DevBuffer,
        gate: &DevBuffer,
        up: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get(Self::glu_kernel(act))?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(out)
            .buf(gate)
            .buf(up)
            .scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Nazwa kernela nieliniowości bramkowanego FFN.
    fn glu_kernel(act: forge_formats::FfnActivation) -> &'static str {
        match act {
            forge_formats::FfnActivation::SiLU => "silu_mul_f16",
            forge_formats::FfnActivation::GeLUTanh => "gelu_mul_f16",
        }
    }

    /// `glu_mul_f16` where gate and up are sections of one fused gate|up
    /// buffer, addressed by byte offsets.
    #[allow(clippy::too_many_arguments)]
    pub fn glu_mul_f16_at(
        &self,
        act: forge_formats::FfnActivation,
        out: &DevBuffer,
        gate_up: &DevBuffer,
        gate_byte_off: usize,
        up_byte_off: usize,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get(Self::glu_kernel(act))?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(out)
            .buf_at(gate_up, gate_byte_off)?
            .buf_at(gate_up, up_byte_off)?
            .scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `h += delta` nad n elementami f16 — strumień rezydualny bez fuzji.
    ///
    /// Silnik nigdy tego nie potrzebował, bo każde dodanie do strumienia
    /// wchodzi u niego w kernel, który tego strumienia dotyka jako następny
    /// (`rmsnorm_residual_f16`, `gemv_residual_*`). Słownictwo operacji ma
    /// `Residual` osobno, więc potrzebuje postaci niescalonej; fuzja przyjdzie
    /// jako pass nad ciągiem operacji, a nie jako założenie wykonawcy.
    pub fn residual_add_f16(
        &self,
        h_io: &DevBuffer,
        delta: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("residual_add_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(h_io).buf(delta).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// buf *= factor w miejscu (skalowanie embeddingu w rodzinie Gemma).
    pub fn scale_f16(&self, buf: &DevBuffer, n: usize, factor: f32, stream: &Stream) -> Result<()> {
        let k = self.artifacts.get("scale_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(buf).scalar(n as i64).scalar(factor);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = f16(src) nad `n` elementami.
    ///
    /// Tensor parallel sumuje wyniki cząstkowe projekcji `down` w f32, bo
    /// dodawanie w f16 gubiłoby bity przy każdej karcie; strumień rezydualny
    /// silnika jest f16. To jedyne miejsce styku tych dwóch reprezentacji.
    pub fn cast_f32_f16(
        &self,
        out: &DevBuffer,
        src: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("cast_f32_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(src).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `out = f16(a + b)` — ogon redukcji podziału kolumnowego na dwie karty.
    pub fn add_f32_out_f16(
        &self,
        out: &DevBuffer,
        a: &DevBuffer,
        b: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        if n == 0 {
            return Err(ForgeError::Kernel("add_f32_out_f16 wymaga n > 0".into()));
        }
        let f32_bytes = checked_buffer_bytes("add_f32_out_f16 wejście", &[n], 4)?;
        let f16_bytes = checked_buffer_bytes("add_f32_out_f16 wyjście", &[n], 2)?;
        if out.len() < f16_bytes || a.len() < f32_bytes || b.len() < f32_bytes {
            return Err(ForgeError::Kernel(
                "add_f32_out_f16: bufor jest mniejszy od wymaganego kształtu".into(),
            ));
        }
        let k = self.artifacts.get("add_f32_out_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(a).buf(b).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// logits = cap * tanh(logits / cap) w miejscu (ograniczenie logitów Gemmy).
    /// `offset` liczony w elementach f32 — głowa batcha zapisuje kolejne lane'y
    /// do jednego bufora.
    pub fn softcap_f32(
        &self,
        logits: &DevBuffer,
        offset: usize,
        n: usize,
        cap: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("softcap_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf_at(logits, offset * std::mem::size_of::<f32>())?
            .scalar(n as i64)
            .scalar(cap);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = a * sigmoid(gate) over n f16 elements (attention output gate).
    pub fn sigmoid_mul_f16(
        &self,
        out: &DevBuffer,
        a: &DevBuffer,
        gate: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("sigmoid_mul_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(a).buf(gate).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// De-interleave a gated Q projection [n_heads, 2*head_dim] into query and
    /// gate halves (each [n_heads, head_dim]). `n = n_heads * head_dim`.
    pub fn deinterleave_gate_f16(
        &self,
        qc: &DevBuffer,
        gatec: &DevBuffer,
        q_full: &DevBuffer,
        head_dim: usize,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("deinterleave_gate_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(qc)
            .buf(gatec)
            .buf(q_full)
            .scalar(head_dim as i64)
            .scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Transformata Hadamarda po wierszu z wynikiem zaokrąglonym do bf16.
    pub fn hadamard_bf16_f16(
        &self,
        buf: &DevBuffer,
        width: usize,
        n_rows: usize,
        stream: &Stream,
    ) -> Result<()> {
        if !width.is_power_of_two() {
            return Err(ForgeError::Kernel(format!(
                "hadamard wymaga szerokości będącej potęgą dwójki, otrzymano {width}"
            )));
        }
        let k = self.artifacts.get("hadamard_bf16_f16")?;
        let threads = (width / 2).next_power_of_two().clamp(32, 512) as u32;
        let cfg = LaunchConfig {
            grid: (n_rows as u32, 1, 1),
            block: (threads, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(buf)
            .scalar(width as i64)
            .scalar(n_rows as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// SwiGLU z niesymetrycznym obcięciem: bramka ograniczana tylko od góry,
    /// wejście obustronnie, oba przed mnożeniem.
    pub fn swiglu_limit_f16(
        &self,
        out: &DevBuffer,
        gate: &DevBuffer,
        up: &DevBuffer,
        n: usize,
        limit: f32,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("swiglu_limit_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(out)
            .buf(gate)
            .buf(up)
            .scalar(n as i64)
            .scalar(limit);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Elementwise GELU (exact erf) over n f16 elements.
    pub fn gelu_f16(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("gelu_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    // --- ONNX subset f32 ops (forge-onnx interpreter) -----------------------

    /// General 1-D convolution (group=1, dilation=1), all f32. `x` [in_ch, in_t],
    /// `w` [out_ch, in_ch, ksize], optional `bias` [out_ch], `out` [out_ch, out_t].
    #[allow(clippy::too_many_arguments)]
    pub fn conv1d_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        w: &DevBuffer,
        bias: Option<&DevBuffer>,
        in_ch: usize,
        in_t: usize,
        out_ch: usize,
        out_t: usize,
        ksize: usize,
        stride: usize,
        pad: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("conv1d_f32")?;
        let cfg = LaunchConfig {
            grid: ((out_t as u32).div_ceil(BLOCK), out_ch as u32, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        // Absent bias still needs a valid device pointer (never read); `out`
        // stands in, mirroring the qkv_post launcher convention.
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .buf(w)
            .buf(bias.unwrap_or(out))
            .scalar(in_ch as i64)
            .scalar(in_t as i64)
            .scalar(out_ch as i64)
            .scalar(out_t as i64)
            .scalar(ksize as i64)
            .scalar(stride as i64)
            .scalar(pad as i64)
            .scalar(if bias.is_some() { 1i64 } else { 0i64 });
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = max(x, 0) over n f32 elements.
    pub fn relu_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("relu_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = sigmoid(x) over n f32 elements.
    pub fn sigmoid_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("sigmoid_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = a + b, same shape, n f32 elements (broadcasting done host-side).
    pub fn add_f32(
        &self,
        out: &DevBuffer,
        a: &DevBuffer,
        b: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("add_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(a).buf(b).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = x^e (elementwise, scalar exponent) over n f32 elements.
    pub fn pow_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        e: f32,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("pow_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(e).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out = sqrt(x) over n f32 elements.
    pub fn sqrt_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("sqrt_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(x).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// out[o, i] = mean over the reduced axis of x viewed as [outer, axis, inner].
    pub fn reduce_mean_f32(
        &self,
        out: &DevBuffer,
        x: &DevBuffer,
        outer: usize,
        axis: usize,
        inner: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("reduce_mean_f32")?;
        let cfg = LaunchConfig::linear((outer * inner) as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf(out)
            .buf(x)
            .scalar(outer as i64)
            .scalar(axis as i64)
            .scalar(inner as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Single-direction, batch-1 ONNX LSTM (gate order i,o,f,c). Shapes are
    /// direction/batch-squeezed by the caller: `x` [seq, input], `w` [4h, input],
    /// `r` [4h, hidden], `b` [8h], `h0`/`c0` [hidden]; `y` [seq, hidden],
    /// `yh`/`yc` [hidden].
    #[allow(clippy::too_many_arguments)]
    pub fn lstm_f32(
        &self,
        y: &DevBuffer,
        yh: &DevBuffer,
        yc: &DevBuffer,
        x: &DevBuffer,
        w: &DevBuffer,
        r: &DevBuffer,
        b: &DevBuffer,
        h0: &DevBuffer,
        c0: &DevBuffer,
        seq: usize,
        input_size: usize,
        hidden: usize,
        stream: &Stream,
    ) -> Result<()> {
        // Shared recurrent state is sized for LSTM_MAX_HIDDEN = 512 in the kernel.
        if hidden > 512 {
            return Err(ForgeError::Kernel(format!(
                "lstm_f32: hidden {hidden} exceeds shared-state capacity (512)"
            )));
        }
        let k = self.artifacts.get("lstm_f32")?;
        let cfg = LaunchConfig {
            grid: (1, 1, 1),
            block: (hidden as u32, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(y)
            .buf(yh)
            .buf(yc)
            .buf(x)
            .buf(w)
            .buf(r)
            .buf(b)
            .buf(h0)
            .buf(c0)
            .scalar(seq as i64)
            .scalar(input_size as i64)
            .scalar(hidden as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
}
