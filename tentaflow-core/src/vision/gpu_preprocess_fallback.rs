// =============================================================================
// Plik: vision/gpu_preprocess_fallback.rs
// Opis: Bezpieczny fallback dla platform bez CUDA preprocessing.
//       Ścieżki CUDA pozostają dostępne typowo, ale zwracają kontrolowany błąd,
//       dzięki czemu pipeline może przejść na preprocessing hosta.
// =============================================================================

use anyhow::{bail, Result};

pub struct DeviceBatch {
    n: usize,
    s: usize,
}

impl DeviceBatch {
    pub fn device_ptr(&self) -> *mut f32 {
        std::ptr::null_mut()
    }

    pub fn n(&self) -> usize {
        self.n
    }

    pub fn s(&self) -> usize {
        self.s
    }

    pub fn elements(&self) -> usize {
        self.n * 3 * self.s * self.s
    }

    pub fn copy_to_host(&self) -> Result<Vec<f32>> {
        bail!("CUDA preprocessing is not enabled")
    }
}

pub fn preprocess_batch_gpu(
    _crops: &[(&[u8], u32, u32)],
    _s: usize,
    _mean: [f32; 3],
    _stdv: [f32; 3],
) -> Result<DeviceBatch> {
    bail!("CUDA preprocessing is not enabled")
}

#[derive(Debug, Clone, Copy)]
pub struct ColorCoeffs {
    pub kr: f32,
    pub kb: f32,
    pub full_range: bool,
}

impl ColorCoeffs {
    pub fn bt709_limited() -> Self {
        Self {
            kr: 0.2126,
            kb: 0.0722,
            full_range: false,
        }
    }

    pub fn bt601_limited() -> Self {
        Self {
            kr: 0.299,
            kb: 0.114,
            full_range: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Nv12Frame<'a> {
    pub y: &'a [u8],
    pub y_stride: usize,
    pub uv: &'a [u8],
    pub uv_stride: usize,
    pub w: u32,
    pub h: u32,
}

pub fn preprocess_nv12_batch_gpu(
    _frames: &[Nv12Frame<'_>],
    _s: usize,
    _mean: [f32; 3],
    _stdv: [f32; 3],
    _color: ColorCoeffs,
) -> Result<DeviceBatch> {
    bail!("CUDA preprocessing is not enabled")
}

pub struct OwnedDeviceTensor {
    n: usize,
    s: usize,
}

impl OwnedDeviceTensor {
    pub fn device_ptr(&self) -> *mut f32 {
        std::ptr::null_mut()
    }

    pub fn n(&self) -> usize {
        self.n
    }

    pub fn s(&self) -> usize {
        self.s
    }

    pub fn elements(&self) -> usize {
        self.n * 3 * self.s * self.s
    }

    pub fn copy_to_host(&self) -> Result<Vec<f32>> {
        bail!("CUDA preprocessing is not enabled")
    }
}

pub fn device_to_host_copy(_device_ptr: u64, _host: &mut [u8]) -> Result<()> {
    bail!("CUDA preprocessing is not enabled")
}

pub struct Nv12CropDownload {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub y_stride: u32,
    pub uv_stride: u32,
    pub uv_offset: u32,
    pub ex0: u32,
    pub ey0: u32,
}

pub fn download_nv12_crop_rect(
    _planes: Nv12DevicePlanes,
    _x0: u32,
    _y0: u32,
    _cw: u32,
    _ch: u32,
) -> Result<Nv12CropDownload> {
    bail!("CUDA preprocessing is not enabled")
}

pub struct Nv12DevicePlanes {
    pub y_ptr: u64,
    pub y_stride: usize,
    pub uv_ptr: u64,
    pub uv_stride: usize,
    pub w: u32,
    pub h: u32,
}

pub fn preprocess_nv12_device_gpu(
    _planes: Nv12DevicePlanes,
    _s: usize,
    _mean: [f32; 3],
    _stdv: [f32; 3],
    _color: ColorCoeffs,
) -> Result<OwnedDeviceTensor> {
    bail!("CUDA preprocessing is not enabled")
}
