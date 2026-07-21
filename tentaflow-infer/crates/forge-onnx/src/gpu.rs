// ===== File: gpu.rs — device staging + kernel dispatch for the ONNX executor =====
//
// Thin wrapper over a HAL `Device` and the forge-kernels launchers. Each method
// takes host f32 slices, uploads them, launches the matching Mojo f32 kernel,
// and downloads the result — so the VAD's real arithmetic (Conv, LSTM,
// activations, magnitude, reduction) executes on the GPU while the interpreter
// keeps values host-resident for the shape/control ops.

use std::sync::Arc;

use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::{MemKind, Result};

pub struct Gpu {
    device: Arc<dyn Device>,
    kernels: Kernels,
    stream: forge_hal::Stream,
}

fn f32_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

fn bytes_to_f32(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

impl Gpu {
    pub fn new(device: Arc<dyn Device>) -> Result<Self> {
        let kernels = Kernels::load(device.clone())?;
        let stream = device.create_stream()?;
        Ok(Self { device, kernels, stream })
    }

    /// Retire the activations ring between forward passes so per-op scratch
    /// buffers do not accumulate across runs.
    pub fn reset(&self) -> Result<()> {
        self.device.reset_activations().map(|_| ())
    }

    fn upload(&self, v: &[f32]) -> Result<DevBuffer> {
        let buf = self
            .device
            .alloc((v.len().max(1)) * 4, MemKind::Device, Pool::Activations)?;
        self.device.write(&f32_to_bytes(v), &buf, 0)?;
        Ok(buf)
    }

    fn alloc(&self, n: usize) -> Result<DevBuffer> {
        self.device
            .alloc(n.max(1) * 4, MemKind::Device, Pool::Activations)
    }

    fn download(&self, buf: &DevBuffer, n: usize) -> Result<Vec<f32>> {
        self.stream.synchronize()?;
        let mut bytes = vec![0u8; n * 4];
        self.device.read(buf, 0, &mut bytes)?;
        Ok(bytes_to_f32(&bytes))
    }

    /// Conv1d: x [in_ch, in_t], w [out_ch, in_ch, ksize], bias [out_ch]?
    /// → out [out_ch, out_t].
    #[allow(clippy::too_many_arguments)]
    pub fn conv1d(
        &self,
        x: &[f32],
        w: &[f32],
        bias: Option<&[f32]>,
        in_ch: usize,
        in_t: usize,
        out_ch: usize,
        out_t: usize,
        ksize: usize,
        stride: usize,
        pad: usize,
    ) -> Result<Vec<f32>> {
        let xb = self.upload(x)?;
        let wb = self.upload(w)?;
        let bias_b = match bias {
            Some(b) => Some(self.upload(b)?),
            None => None,
        };
        let out = self.alloc(out_ch * out_t)?;
        self.kernels.conv1d_f32(
            &out,
            &xb,
            &wb,
            bias_b.as_ref(),
            in_ch,
            in_t,
            out_ch,
            out_t,
            ksize,
            stride,
            pad,
            &self.stream,
        )?;
        self.download(&out, out_ch * out_t)
    }

    pub fn relu(&self, x: &[f32]) -> Result<Vec<f32>> {
        let xb = self.upload(x)?;
        let out = self.alloc(x.len())?;
        self.kernels.relu_f32(&out, &xb, x.len(), &self.stream)?;
        self.download(&out, x.len())
    }

    pub fn sigmoid(&self, x: &[f32]) -> Result<Vec<f32>> {
        let xb = self.upload(x)?;
        let out = self.alloc(x.len())?;
        self.kernels.sigmoid_f32(&out, &xb, x.len(), &self.stream)?;
        self.download(&out, x.len())
    }

    /// Elementwise add of equal-length buffers (broadcast expanded host-side).
    pub fn add(&self, a: &[f32], b: &[f32]) -> Result<Vec<f32>> {
        let ab = self.upload(a)?;
        let bb = self.upload(b)?;
        let out = self.alloc(a.len())?;
        self.kernels.add_f32(&out, &ab, &bb, a.len(), &self.stream)?;
        self.download(&out, a.len())
    }

    pub fn pow(&self, x: &[f32], e: f32) -> Result<Vec<f32>> {
        let xb = self.upload(x)?;
        let out = self.alloc(x.len())?;
        self.kernels.pow_f32(&out, &xb, e, x.len(), &self.stream)?;
        self.download(&out, x.len())
    }

    pub fn sqrt(&self, x: &[f32]) -> Result<Vec<f32>> {
        let xb = self.upload(x)?;
        let out = self.alloc(x.len())?;
        self.kernels.sqrt_f32(&out, &xb, x.len(), &self.stream)?;
        self.download(&out, x.len())
    }

    /// Reduce-mean over the middle axis of an [outer, axis, inner] view.
    pub fn reduce_mean(
        &self,
        x: &[f32],
        outer: usize,
        axis: usize,
        inner: usize,
    ) -> Result<Vec<f32>> {
        let xb = self.upload(x)?;
        let out = self.alloc(outer * inner)?;
        self.kernels
            .reduce_mean_f32(&out, &xb, outer, axis, inner, &self.stream)?;
        self.download(&out, outer * inner)
    }

    /// Single-direction batch-1 LSTM. Returns (Y [seq*hidden], Y_h [hidden],
    /// Y_c [hidden]).
    #[allow(clippy::too_many_arguments)]
    pub fn lstm(
        &self,
        x: &[f32],
        w: &[f32],
        r: &[f32],
        b: &[f32],
        h0: &[f32],
        c0: &[f32],
        seq: usize,
        input_size: usize,
        hidden: usize,
    ) -> Result<(Vec<f32>, Vec<f32>, Vec<f32>)> {
        let xb = self.upload(x)?;
        let wb = self.upload(w)?;
        let rb = self.upload(r)?;
        let bb = self.upload(b)?;
        let h0b = self.upload(h0)?;
        let c0b = self.upload(c0)?;
        let y = self.alloc(seq * hidden)?;
        let yh = self.alloc(hidden)?;
        let yc = self.alloc(hidden)?;
        self.kernels.lstm_f32(
            &y,
            &yh,
            &yc,
            &xb,
            &wb,
            &rb,
            &bb,
            &h0b,
            &c0b,
            seq,
            input_size,
            hidden,
            &self.stream,
        )?;
        Ok((
            self.download(&y, seq * hidden)?,
            self.download(&yh, hidden)?,
            self.download(&yc, hidden)?,
        ))
    }
}
