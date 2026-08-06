// ===== File: shape.rs — tensor shapes with row-major strides =====

use serde::{Deserialize, Serialize};

/// Tensor shape, row-major (last dim contiguous). Rank ≤ 4 covers every op in
/// the v1 IR (batch, heads, seq, dim).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Shape(Vec<usize>);

impl Shape {
    pub fn new(dims: impl Into<Vec<usize>>) -> Self {
        Shape(dims.into())
    }

    pub fn rank(&self) -> usize {
        self.0.len()
    }

    /// Element count with overflow detection. Dimensions can come from
    /// untrusted model headers, so wrapping multiplication must never drive an
    /// allocation size.
    pub fn checked_numel(&self) -> Option<usize> {
        self.0.iter().try_fold(1usize, |acc, &d| acc.checked_mul(d))
    }

    /// Element count for shapes already validated by the format layer.
    /// Panics (also in release) if the product overflows.
    pub fn numel(&self) -> usize {
        self.checked_numel()
            .expect("shape element count overflows usize")
    }

    pub fn dims(&self) -> &[usize] {
        &self.0
    }
}

impl std::fmt::Display for Shape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}]",
            self.0
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join("x")
        )
    }
}

/// Wymiary architektury gęstej.
///
/// Mieszkają tu, a nie przy modelu, bo potrzebują ich OBIE strony granicy
/// sprzętowej: model, żeby opisać kolejność operacji, i wykonawca, żeby
/// wymiarować bufory. Gdyby leżały wyżej, wykonawca musiałby sięgnąć w górę —
/// czego układ zależności zabrania i słusznie.
///
/// Nie ma tu rozmiaru grupy kwantyzacji. To NIE jest własność architektury,
/// tylko pojedynczej wagi: Q4_K_M trzyma 32 na większości i 16 na tych w Q6_K.
/// Póki było tu polem, kernel osadzeń dostawał grupę modelu zamiast swojej i
/// działał tylko dlatego, że akurat się zgadzały.
#[derive(Debug, Clone, Copy)]
pub struct DenseShape {
    pub hidden: u32,
    pub layers: u32,
    pub heads: u32,
    pub kv_heads: u32,
    pub head_dim: u32,
    pub inter: u32,
    pub vocab: u32,
    pub eps: f32,
    pub rope_theta: f32,
}

impl DenseShape {
    pub fn kv_width(&self) -> u32 {
        self.kv_heads * self.head_dim
    }

    /// The width of Q and of the attention output — heads times head width.
    ///
    /// This is NOT `hidden`, although llama and Bielik make them equal.
    /// Qwen3-MoE has 32 heads of 128 against a hidden of 2048, so its Q
    /// projection is twice the width of the residual stream and the output
    /// projection narrows it back. While the two numbers were confused into
    /// one, every such checkpoint sized half of the attention buffers.
    pub fn attn_width(&self) -> u32 {
        self.heads * self.head_dim
    }

    pub fn attn_scale(&self) -> f32 {
        1.0 / (self.head_dim as f32).sqrt()
    }
}
