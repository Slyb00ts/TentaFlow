// ===== File: gemm_fp4.rs — GEMM z czterema bitami po obu stronach =====
//
// Dwa pytania, i mylenie ich jest tu najłatwiejszym błędem do popełnienia.
//
// PIERWSZE: czy kernel liczy iloczyn tych liczb, które dostał. Odpowiada na nie
// porównanie z wzorcem policzonym na hoście z ODCZYTANYCH Z KARTY kodów — więc
// kwantyzacja aktywacji nie jest w tym błędzie w ogóle. Próg jest ciasny.
//
// DRUGIE: ile kosztuje sama kwantyzacja aktywacji do czterech bitów. To jest
// cena tej ścieżki i nie da się jej zmniejszyć lepszym kernelem; mierzy się ją
// wobec produktu f32 z wejść PRZED kwantyzacją i raportuje liczbą, a nie
// zdaniem.

use std::sync::Arc;

use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

const ROWS: usize = 192;
const COLS: usize = 512;
const TOKENS: usize = 160;

fn device() -> Option<Arc<dyn Device>> {
    forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 64 << 20,
            kv_cache: 4 << 20,
            activations: 64 << 20,
            kv_page_size: 256 << 10,
        },
    )
    .map_err(|e| eprintln!("pomijam GEMM fp4: {e}"))
    .ok()
}

const E2M1: [f32; 8] = [0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0];

fn e2m1(code: u8) -> f32 {
    let v = E2M1[(code & 0x7) as usize];
    if code & 0x8 != 0 {
        -v
    } else {
        v
    }
}

fn e4m3(code: u8) -> f32 {
    let exponent = i32::from((code >> 3) & 0x0F);
    let mantissa = f32::from(code & 0x07) / 8.0;
    let value = if exponent == 0 {
        mantissa * (2f32).powi(-6)
    } else {
        (1.0 + mantissa) * (2f32).powi(exponent - 7)
    };
    if code & 0x80 != 0 {
        -value
    } else {
        value
    }
}

/// One row of a NVFP4 block stream, decoded exactly as the instruction reads it.
///
/// Byte `j` of a sixteen carries element `j` in the low nibble and `j + 8` in
/// the high one — the GGUF interleave, which the activation packer reproduces
/// so both operands permute k the same way.
fn decode_row(blocks: &[u8], cols: usize) -> Vec<f32> {
    let mut out = vec![0f32; cols];
    for b in 0..cols / 64 {
        let base = b * 36;
        for sub in 0..4 {
            let scale = e4m3(blocks[base + sub]);
            for j in 0..8 {
                let byte = blocks[base + 4 + sub * 8 + j];
                out[b * 64 + sub * 16 + j] = e2m1(byte & 0x0F) * scale;
                out[b * 64 + sub * 16 + j + 8] = e2m1(byte >> 4) * scale;
            }
        }
    }
    out
}

fn upload(dev: &dyn Device, bytes: &[u8], pool: Pool) -> DevBuffer {
    let buf = dev.alloc(bytes.len(), MemKind::Device, pool).unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

/// Weights in the GGUF NVFP4 layout, with scales and codes that vary in both
/// directions — a kernel that transposed a fragment would still pass on a
/// weight whose rows were all alike.
fn weights() -> Vec<u8> {
    let mut w = vec![0u8; ROWS * COLS / 64 * 36];
    for row in 0..ROWS {
        for b in 0..COLS / 64 {
            let base = (row * COLS / 64 + b) * 36;
            for sub in 0..4 {
                // Exponent field only, so the decoded scale is a power of two.
                w[base + sub] = (((row + b + sub) % 5 + 5) << 3) as u8;
                for j in 0..8 {
                    let lo = (row * 3 + b * 5 + sub * 7 + j) % 16;
                    let hi = (row * 11 + b + sub * 3 + j * 5) % 16;
                    w[base + 4 + sub * 8 + j] = (lo | (hi << 4)) as u8;
                }
            }
        }
    }
    w
}

#[test]
fn the_four_bit_gemm_computes_what_it_was_given() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    if !kernels.supports_mxf4_block_scale() {
        eprintln!("pomijam: brak artefaktow blokowo-skalowanego FP4");
        return;
    }
    let stream = dev.create_stream().unwrap();

    let w = weights();
    let w_dev = upload(dev.as_ref(), &w, Pool::Weights);

    // Activations spanning three orders of magnitude within a token, which is
    // what makes the per-token global scale earn its place.
    let x: Vec<f16> = (0..TOKENS * COLS)
        .map(|i| {
            let t = i / COLS;
            let c = i % COLS;
            let mag = (2f32).powi(((c / 64 + t) % 7) as i32 - 3);
            f16::from_f32(mag * ((((c * 13 + t * 7) % 29) as f32 - 14.0) / 14.0))
        })
        .collect();
    let x_dev = upload(dev.as_ref(), bytemuck::cast_slice(&x), Pool::Activations);

    let block_bytes = COLS / 64 * 36;
    let xq = dev
        .alloc(TOKENS * block_bytes, MemKind::Device, Pool::Activations)
        .unwrap();
    let xs = dev
        .alloc(TOKENS * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let y = dev
        .alloc(TOKENS * ROWS * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    // A tensor-wide multiplier that is not 1, so a kernel that dropped it fails.
    let output_scale = 0.375f32;
    kernels
        .quantize_act_nvfp4(&xq, &xs, &x_dev, COLS, TOKENS, &stream)
        .unwrap();
    kernels
        .gemm_nvfp4_mma_f16(
            &y,
            &w_dev,
            &xq,
            &xs,
            ROWS,
            COLS,
            TOKENS,
            output_scale,
            &stream,
        )
        .unwrap();
    stream.synchronize().unwrap();

    let mut xq_host = vec![0u8; TOKENS * block_bytes];
    dev.read(&xq, 0, &mut xq_host).unwrap();
    let mut xs_bytes = vec![0u8; TOKENS * 4];
    dev.read(&xs, 0, &mut xs_bytes).unwrap();
    let xs_host: &[f32] = bytemuck::cast_slice(&xs_bytes);
    let mut y_bytes = vec![0u8; TOKENS * ROWS * 2];
    dev.read(&y, 0, &mut y_bytes).unwrap();
    let got: Vec<f32> = y_bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect();

    let w_rows: Vec<Vec<f32>> = (0..ROWS)
        .map(|r| decode_row(&w[r * block_bytes..(r + 1) * block_bytes], COLS))
        .collect();

    let mut worst_kernel = 0f32;
    let mut worst_quant = 0f32;
    let mut span = 0f32;
    for t in 0..TOKENS {
        let xt = decode_row(&xq_host[t * block_bytes..(t + 1) * block_bytes], COLS);
        for r in 0..ROWS {
            // What the kernel was given, added up on the host.
            let want: f32 =
                (0..COLS).map(|c| w_rows[r][c] * xt[c]).sum::<f32>() * output_scale * xs_host[t];
            // What it would have been without quantizing the activation.
            let ideal: f32 = (0..COLS)
                .map(|c| w_rows[r][c] * x[t * COLS + c].to_f32())
                .sum::<f32>()
                * output_scale;
            let g = got[t * ROWS + r];
            span = span.max(want.abs());
            worst_kernel = worst_kernel.max((g - want).abs());
            worst_quant = worst_quant.max((g - ideal).abs());
        }
    }

    eprintln!(
        "blad kernela {:.4}% rozpietosci, koszt kwantyzacji aktywacji {:.4}%",
        100.0 * worst_kernel / span,
        100.0 * worst_quant / span
    );
    assert!(
        span > 1.0,
        "wynik jest praktycznie zerowy: rozpietosc {span}"
    );
    // Only f16 rounding of the result and the order the accumulator adds in
    // separate the two sides; anything larger is a fragment or a scale in the
    // wrong place, and that lands orders of magnitude outside.
    assert!(
        worst_kernel / span < 5e-3,
        "kernel rozjechal sie z wzorcem o {:.4}% rozpietosci",
        100.0 * worst_kernel / span
    );
}

/// A token count under one tile and a row count that is not a multiple of it.
///
/// The guard clauses are the whole point: a tile that wrote its out-of-range
/// lanes would corrupt the next row of the output, which the shape above — a
/// multiple of neither block dimension — would hide.
#[test]
fn the_four_bit_gemm_respects_a_ragged_shape() {
    let Some(dev) = device() else { return };
    let kernels = Kernels::load(dev.clone()).unwrap();
    if !kernels.supports_mxf4_block_scale() {
        eprintln!("pomijam: brak artefaktow blokowo-skalowanego FP4");
        return;
    }
    let stream = dev.create_stream().unwrap();

    let rows = 37usize;
    let tokens = 11usize;
    let cols = 128usize;
    let block_bytes = cols / 64 * 36;

    let mut w = vec![0u8; rows * block_bytes];
    for (i, byte) in w.iter_mut().enumerate() {
        *byte = if i % 36 < 4 {
            0x38
        } else {
            ((i * 7) % 256) as u8
        };
    }
    let w_dev = upload(dev.as_ref(), &w, Pool::Weights);
    let x: Vec<f16> = (0..tokens * cols)
        .map(|i| f16::from_f32(((i % 17) as f32 - 8.0) / 8.0))
        .collect();
    let x_dev = upload(dev.as_ref(), bytemuck::cast_slice(&x), Pool::Activations);

    let xq = dev
        .alloc(tokens * block_bytes, MemKind::Device, Pool::Activations)
        .unwrap();
    let xs = dev
        .alloc(tokens * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    // One extra element, pre-filled, to catch a write past the last row.
    let y = dev
        .alloc((tokens * rows + 1) * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(&vec![0x5Au8; (tokens * rows + 1) * 2], &y, 0)
        .unwrap();

    kernels
        .quantize_act_nvfp4(&xq, &xs, &x_dev, cols, tokens, &stream)
        .unwrap();
    kernels
        .gemm_nvfp4_mma_f16(&y, &w_dev, &xq, &xs, rows, cols, tokens, 1.0, &stream)
        .unwrap();
    stream.synchronize().unwrap();

    let mut bytes = vec![0u8; (tokens * rows + 1) * 2];
    dev.read(&y, 0, &mut bytes).unwrap();
    assert_eq!(
        &bytes[tokens * rows * 2..],
        &[0x5A, 0x5A],
        "kafel zapisal poza ostatnim wierszem wyjscia"
    );

    let mut xq_host = vec![0u8; tokens * block_bytes];
    dev.read(&xq, 0, &mut xq_host).unwrap();
    let mut xs_bytes = vec![0u8; tokens * 4];
    dev.read(&xs, 0, &mut xs_bytes).unwrap();
    let xs_host: &[f32] = bytemuck::cast_slice(&xs_bytes);
    let got: Vec<f32> = bytes[..tokens * rows * 2]
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect();

    let mut worst = 0f32;
    let mut span = 0f32;
    for t in 0..tokens {
        let xt = decode_row(&xq_host[t * block_bytes..(t + 1) * block_bytes], cols);
        for r in 0..rows {
            let wr = decode_row(&w[r * block_bytes..(r + 1) * block_bytes], cols);
            let want: f32 = (0..cols).map(|c| wr[c] * xt[c]).sum::<f32>() * xs_host[t];
            span = span.max(want.abs());
            worst = worst.max((got[t * rows + r] - want).abs());
        }
    }
    assert!(span > 1e-3, "wynik jest praktycznie zerowy");
    assert!(
        worst / span < 5e-3,
        "ksztalt nieregularny rozjechal sie o {:.4}% rozpietosci",
        100.0 * worst / span
    );
}
