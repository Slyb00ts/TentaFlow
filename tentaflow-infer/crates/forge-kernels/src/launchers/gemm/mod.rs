// ===== File: gemm/mod.rs — mnozenie macierzy, po formacie wag =====
use super::*;

mod dense;
mod fp8;
pub mod mxf4;
mod nvfp4;
mod quantized;
pub use quantized::q4k_decode_profile::Q4kDecodeModelFamily;
