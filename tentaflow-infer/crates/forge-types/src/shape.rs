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
