// ===== File: metal_q4k.rs — raw Q4_K matrix units against affine Q4_K =====

#![cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]

use std::sync::Arc;

use forge_formats::affine::{to_affine_triple, AffineTriple};
use forge_graph::{
    Act, ExecSpec, Executor, Layout, Op, PackedWeight, Planes, QuantWeight, Step, WeightStore,
};
use forge_hal::metal_device::MetalDevice;
use forge_kernels::MetalExec;
use forge_types::{DType, DenseShape, QuantKind};

const ROWS: usize = 4096;
const COLS: usize = 11_264;
const TOKENS: usize = 256;

fn spec() -> ExecSpec {
    ExecSpec {
        shape: DenseShape {
            hidden: COLS as u32,
            layers: 1,
            heads: 1,
            kv_heads: 1,
            head_dim: 64,
            inter: ROWS as u32,
            vocab: 1,
            eps: 1e-5,
            rope_theta: 10_000.0,
            rope_rot: 64,
        },
        attends: vec![true].into(),
        quant_params: DType::F16,
        norm_weights: DType::F32,
        ssm: None,
    }
}

fn q4_k_bytes() -> Vec<u8> {
    let mut data = vec![0u8; ROWS * COLS / 256 * 144];
    for (i, byte) in data.iter_mut().enumerate() {
        *byte = (i as u8).wrapping_mul(17).wrapping_add(3);
    }
    for block in data.chunks_exact_mut(144) {
        block[0..2].copy_from_slice(&0x2E66u16.to_le_bytes());
        block[2..4].copy_from_slice(&0u16.to_le_bytes());
        block[4..16].fill(1);
    }
    data
}

fn embedding() -> AffineTriple {
    let mut table = AffineTriple::new_f16(1, COLS, 32);
    for col in 0..COLS {
        table.put(0, col, (col % 16) as u8);
    }
    for group in 0..COLS / 32 {
        table.set_params_f16(0, group, 0.03125, -0.2);
    }
    table
}

fn run(mut exec: MetalExec, weight: QuantWeight, cpu_share: bool) -> Vec<f32> {
    exec.set_cpu_share(cpu_share);
    let table = exec
        .put_quant(QuantWeight::Affine(embedding()))
        .expect("embedding");
    let projection = exec.put_quant(weight).expect("Q4_K projection");
    let step = Step::single(0, 0, TOKENS as u32).expect("step");
    exec.run(&Op::Embed {
        table,
        tokens: vec![0; TOKENS],
        step: step.clone(),
    })
    .expect("embed");
    exec.run(&Op::MatMul {
        out: Act::Gate,
        w: projection,
        x: Act::Hidden,
        step,
    })
    .expect("matmul");
    exec.read(Act::Gate, TOKENS * ROWS).expect("read")
}

#[test]
#[ignore]
fn raw_q4_k_matrix_units_match_affine_q4_k() {
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let raw = q4_k_bytes();
    let affine = to_affine_triple(&raw, QuantKind::Q4K, ROWS, COLS).expect("affine");
    let raw_weight = QuantWeight::Packed(PackedWeight {
        planes: Planes {
            codes: raw,
            ..Planes::default()
        },
        quant: QuantKind::Q4K,
        layout: Layout::Blocks,
        dtype: DType::U8,
        rows: ROWS,
        cols: COLS,
    });
    let raw_out = run(
        MetalExec::new(device.clone() as Arc<_>, spec()).expect("raw exec"),
        raw_weight,
        false,
    );
    let affine_out = run(
        MetalExec::new(device.clone() as Arc<_>, spec()).expect("affine exec"),
        QuantWeight::Affine(affine),
        false,
    );
    let affine_shared_out = run(
        MetalExec::new(device.clone() as Arc<_>, spec()).expect("affine shared exec"),
        QuantWeight::Affine(
            to_affine_triple(&q4_k_bytes(), QuantKind::Q4K, ROWS, COLS).expect("affine shared"),
        ),
        true,
    );
    let shared_out = run(
        MetalExec::new(device as Arc<_>, spec()).expect("shared exec"),
        QuantWeight::Packed(PackedWeight {
            planes: Planes {
                codes: q4_k_bytes(),
                ..Planes::default()
            },
            quant: QuantKind::Q4K,
            layout: Layout::Blocks,
            dtype: DType::U8,
            rows: ROWS,
            cols: COLS,
        }),
        true,
    );
    let shared_worst = raw_out
        .iter()
        .zip(shared_out)
        .map(|(&gpu, shared)| (gpu - shared).abs())
        .fold(0.0f32, f32::max);
    let affine_worst = raw_out
        .iter()
        .zip(&affine_out)
        .map(|(&raw, affine)| (raw - affine).abs())
        .fold(0.0f32, f32::max);
    eprintln!("raw Q4_K vs affine Q4_K: max {affine_worst:.5}");
    assert!(
        affine_worst < 0.05,
        "raw Q4_K QMG odjechał od affine o {affine_worst}"
    );
    let affine_shared_worst = affine_out
        .iter()
        .zip(affine_shared_out)
        .map(|(&gpu, shared)| (gpu - shared).abs())
        .fold(0.0f32, f32::max);
    eprintln!("affine Q4_K CPU-share vs GPU-only: max {affine_shared_worst:.5}");
    eprintln!("raw Q4_K CPU-share vs GPU-only: max {shared_worst:.5}");
    assert!(
        shared_worst < 0.05,
        "raw Q4_K CPU-share odjechał od GPU-only o {shared_worst}"
    );
}
