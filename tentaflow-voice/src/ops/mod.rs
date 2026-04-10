// =============================================================================
// Plik: ops/mod.rs
// Opis: Operacje neural network — pure Rust + SIMD przez `wide`.
//       Wszystkie funkcje zaoptymalizowane pod hot loopy (conv1d, matmul).
// =============================================================================

pub mod activation;
pub mod batch_norm;
pub mod conv1d;
pub mod elementwise;
pub mod gemm;
pub mod linear;
pub mod lstm;
pub mod matmul;
pub mod reduce;
pub mod softmax;

pub use activation::{relu, relu_inplace, sigmoid, sigmoid_scalar, tanh_f32};
pub use batch_norm::BatchNorm1dFused;
pub use conv1d::{conv1d_naive, conv1d_simd, Conv1dParams};
pub use elementwise::{add, add_inplace, l2_normalize, mul, mul_inplace, mul_scalar_inplace, sqrt_with_eps};
pub use gemm::{gemm, gemm_accumulate, gemv};
pub use linear::{linear, linear_bias};
pub use lstm::{LstmCell, LstmState};
pub use matmul::{matvec_f32, matvec_f32_simd};
pub use reduce::{mean_axis_last, sum_axis_last, weighted_mean, weighted_std};
pub use softmax::softmax_axis_last;
