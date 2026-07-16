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

    pub fn numel(&self) -> usize {
        self.0.iter().product()
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
            self.0.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("x")
        )
    }
}
