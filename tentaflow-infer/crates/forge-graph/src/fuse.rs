// ===== File: fuse.rs — structural fusion of dense operations =====
use crate::{Act, Op};

/// Rewrites adjacent dense operations into fused vocabulary operations.
///
/// The pass is deliberately backend-independent. Executors may use a fused kernel
/// or execute the fused operation as its component operations when they lack one.
pub fn fuse(ops: &[Op]) -> Vec<Op> {
    let mut out = Vec::with_capacity(ops.len());
    let mut i = 0;
    while i < ops.len() {
        if let Some((fused, consumed)) = try_fuse_norm_matmul(&ops[i..]) {
            out.push(fused);
            i += consumed;
            continue;
        }
        if let Some((fused, consumed)) = try_fuse_matmul_residual(&ops[i..]) {
            out.push(fused);
            i += consumed;
            continue;
        }
        out.push(ops[i].clone());
        i += 1;
    }
    out
}

fn try_fuse_norm_matmul(ops: &[Op]) -> Option<(Op, usize)> {
    let [Op::RmsNorm {
        out: norm_out,
        x,
        w: norm_w,
        step: norm_step,
    }, Op::MatMul {
        out,
        w,
        x: matmul_x,
        step,
    }, ..] = ops
    else {
        return None;
    };
    if norm_out != matmul_x || *norm_step != *step || count_consumers(ops, *norm_out) != 1 {
        return None;
    }
    Some((
        Op::FusedNormMatMul {
            out: *out,
            w: *w,
            norm_w: *norm_w,
            x: *x,
            step: step.clone(),
        },
        2,
    ))
}

fn count_consumers(ops: &[Op], act: Act) -> usize {
    ops.iter()
        .map(|op| match op {
            Op::MatMul { x, .. } if *x == act => 1,
            Op::RmsNorm { x, .. } if *x == act => 1,
            Op::Residual { src, .. } if *src == act => 1,
            Op::LogitsOfLast { x, .. } if *x == act => 1,
            // Czyta swój slot i pisze do niego z powrotem, więc jest jego
            // czytelnikiem. Pominięcie go tutaj pozwoliłoby scalić parę, między
            // którą ktoś jeszcze zagląda do tego slotu.
            Op::HeadNorm { act: a, .. } if *a == act => 1,
            _ => 0,
        })
        .sum()
}

fn try_fuse_matmul_residual(ops: &[Op]) -> Option<(Op, usize)> {
    let [Op::MatMul {
        out,
        w,
        x,
        step: matmul_step,
    }, Op::Residual { src, step }, ..] = ops
    else {
        return None;
    };
    if out != src || matmul_step != step || count_consumers(ops, *out) != 1 {
        return None;
    }
    Some((
        Op::FusedMatMulResidual {
            w: *w,
            x: *x,
            step: step.clone(),
        },
        2,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Step, WeightId};

    fn step() -> Step {
        Step::single(0, 0, 1).unwrap()
    }

    #[test]
    fn fuses_norm_matmul() {
        let s = step();
        let ops = vec![
            Op::RmsNorm {
                out: Act::Norm,
                x: Act::Hidden,
                w: WeightId(1),
                step: s.clone(),
            },
            Op::MatMul {
                out: Act::Query,
                w: WeightId(2),
                x: Act::Norm,
                step: s,
            },
        ];
        assert!(matches!(fuse(&ops)[0], Op::FusedNormMatMul { .. }));
    }

    #[test]
    fn fuses_matmul_residual() {
        let s = step();
        let ops = vec![
            Op::MatMul {
                out: Act::Proj,
                w: WeightId(1),
                x: Act::Attn,
                step: s.clone(),
            },
            Op::Residual {
                src: Act::Proj,
                step: s,
            },
        ];
        assert!(matches!(fuse(&ops)[0], Op::FusedMatMulResidual { .. }));
    }

    /// A norm read by TWO projections must survive the pass untouched.
    ///
    /// This is the FFN chain, and the fused form is a norm folded into the
    /// projection that reads it. Folding it into the gate would leave the up
    /// projection reading a slot nobody wrote this step — stale activations
    /// from the previous layer, which is fluent output for someone else's
    /// context rather than a failure. The consumer count is what refuses it,
    /// so it is the thing worth a test.
    #[test]
    fn leaves_a_norm_with_two_readers_alone() {
        let s = step();
        let ops = vec![
            Op::RmsNorm {
                out: Act::Norm,
                x: Act::Hidden,
                w: WeightId(1),
                step: s.clone(),
            },
            Op::MatMul {
                out: Act::Gate,
                w: WeightId(2),
                x: Act::Norm,
                step: s.clone(),
            },
            Op::MatMul {
                out: Act::Up,
                w: WeightId(3),
                x: Act::Norm,
                step: s.clone(),
            },
            Op::SiluMul { step: s },
        ];
        let fused = fuse(&ops);
        assert_eq!(fused.len(), 4, "chain with a shared norm was rewritten");
        assert!(matches!(fused[0], Op::RmsNorm { .. }));
    }

    #[test]
    fn leaves_nonmatching_ops_unchanged() {
        let s = step();
        let ops = vec![Op::RmsNorm {
            out: Act::Norm,
            x: Act::Hidden,
            w: WeightId(1),
            step: s,
        }];
        assert!(matches!(fuse(&ops)[0], Op::RmsNorm { .. }));
    }
}
