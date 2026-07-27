// ===== File: deepseek_activations.rs — kernele aktywacji DeepSeeka V4 na GPU =====
//
// Pięć operacji, których nie ma w pozostałych architekturach. Referencje CPU są
// tu powtórzone celowo: te same wzory zostały wcześniej przypięte do
// implementacji autorów modelu na prawdziwych wagach
// (`forge-formats/tests/deepseek_v4_attention.rs`), więc zgodność kernela z
// referencją CPU domyka łańcuch aż do modelu.

use std::sync::Arc;

use forge_hal::cuda::PoolSizes;
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

/// Test MUSI biec na realnym urządzeniu — brak GPU to błąd, nie pominięcie.
fn device() -> Arc<dyn Device> {
    forge_hal::gpu::open(
        0,
        PoolSizes {
            weights: 64 << 20,
            kv_cache: 16 << 20,
            activations: 64 << 20,
            kv_page_size: 256 << 10,
        },
    )
    .expect("GPU wymagane")
}

fn upload_f16(dev: &dyn Device, values: &[f32]) -> DevBuffer {
    let host: Vec<f16> = values.iter().map(|v| f16::from_f32(*v)).collect();
    let bytes = unsafe { std::slice::from_raw_parts(host.as_ptr() as *const u8, host.len() * 2) };
    let buf = dev
        .alloc(bytes.len(), MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn download_f16(dev: &dyn Device, buf: &DevBuffer, n: usize) -> Vec<f32> {
    let mut bytes = vec![0u8; n * 2];
    dev.read(buf, 0, &mut bytes).unwrap();
    bytes
        .chunks_exact(2)
        .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
        .collect()
}

/// Wartości f16 po drodze, więc porównujemy z tolerancją f16, nie f32.
fn assert_close(got: &[f32], want: &[f32], tol: f32, what: &str) {
    let mut worst = 0f32;
    for (g, w) in got.iter().zip(want) {
        worst = worst.max((g - w).abs() / w.abs().max(1e-3));
    }
    assert!(worst < tol, "{what}: największy błąd względny {worst:.3e}");
    assert!(
        want.iter().any(|v| v.abs() > 1e-3),
        "{what}: referencja jest zerowa, test nic nie dowodzi"
    );
}

fn pattern(n: usize, seed: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (((i * seed + 13) % 197) as f32 - 98.0) * 0.031)
        .collect()
}

#[test]
fn rmsnorm_head_normalizes_each_head_without_weight() {
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (head_dim, n_heads, eps) = (512usize, 5usize, 1e-6f32);
    let values = pattern(head_dim * n_heads, 37);
    let buf = upload_f16(dev.as_ref(), &values);

    kernels
        .rmsnorm_head_f16(&buf, head_dim, n_heads, eps, &stream)
        .unwrap();
    stream.synchronize().unwrap();
    let got = download_f16(dev.as_ref(), &buf, values.len());

    let mut want = Vec::with_capacity(values.len());
    for head in 0..n_heads {
        let slot = &values[head * head_dim..(head + 1) * head_dim];
        // Wejście przeszło przez f16, więc referencja też musi.
        let rounded: Vec<f32> = slot.iter().map(|v| f16::from_f32(*v).to_f32()).collect();
        let mean = rounded.iter().map(|v| v * v).sum::<f32>() / head_dim as f32;
        let inv = (mean + eps).sqrt().recip();
        want.extend(rounded.iter().map(|v| v * inv));
    }
    assert_close(&got, &want, 2e-3, "rmsnorm per głowica");
}

#[test]
fn rope_interleaved_rotates_adjacent_pairs() {
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (head_dim, rope_dim, n_rows) = (512usize, 64usize, 4usize);
    let offset = head_dim - rope_dim;
    let values = pattern(head_dim * n_rows, 53);
    let freqs: Vec<f32> = (0..rope_dim / 2)
        .map(|i| 1.0 / 160_000f32.powf(2.0 * i as f32 / rope_dim as f32))
        .collect();
    let freq_bytes = unsafe {
        std::slice::from_raw_parts(freqs.as_ptr() as *const u8, freqs.len() * 4)
    };
    let freq_buf = dev
        .alloc(freq_bytes.len(), MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(freq_bytes, &freq_buf, 0).unwrap();

    for inverse in [false, true] {
        let buf = upload_f16(dev.as_ref(), &values);
        kernels
            .rope_interleaved_f16(
                &buf, &freq_buf, head_dim, offset, rope_dim, n_rows, 0, 1, inverse, &stream,
            )
            .unwrap();
        stream.synchronize().unwrap();
        let got = download_f16(dev.as_ref(), &buf, values.len());

        let mut want: Vec<f32> = values.iter().map(|v| f16::from_f32(*v).to_f32()).collect();
        for row in 0..n_rows {
            for (i, freq) in freqs.iter().enumerate() {
                let angle = row as f32 * freq;
                let (mut sin, cos) = angle.sin_cos();
                if inverse {
                    sin = -sin;
                }
                let at = row * head_dim + offset + 2 * i;
                let (a, b) = (want[at], want[at + 1]);
                want[at] = a * cos - b * sin;
                want[at + 1] = a * sin + b * cos;
            }
        }
        assert_close(&got, &want, 3e-3, if inverse { "rope odwrotne" } else { "rope" });

        // Wymiary poza wycinkiem muszą zostać nietknięte.
        for row in 0..n_rows {
            for col in 0..offset {
                let at = row * head_dim + col;
                assert_eq!(
                    got[at],
                    f16::from_f32(values[at]).to_f32(),
                    "rope ruszyło wymiar {col} spoza swojego wycinka"
                );
            }
        }
    }
}

#[test]
fn hadamard_matches_the_cpu_transform() {
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (width, n_rows) = (128usize, 3usize);
    let values = pattern(width * n_rows, 71);
    let buf = upload_f16(dev.as_ref(), &values);

    kernels
        .hadamard_bf16_f16(&buf, width, n_rows, &stream)
        .unwrap();
    stream.synchronize().unwrap();
    let got = download_f16(dev.as_ref(), &buf, values.len());

    let mut want = Vec::with_capacity(values.len());
    for row in 0..n_rows {
        let mut work: Vec<f32> = values[row * width..(row + 1) * width]
            .iter()
            .map(|v| f16::from_f32(*v).to_f32())
            .collect();
        let mut step = 1;
        while step < width {
            for base in (0..width).step_by(2 * step) {
                for i in 0..step {
                    let (a, b) = (work[base + i], work[base + step + i]);
                    work[base + i] = a + b;
                    work[base + step + i] = a - b;
                }
            }
            step *= 2;
        }
        let scale = (width as f32).sqrt().recip();
        // Zaokrąglenie do bf16 jest częścią kontraktu, nie kosmetyką.
        want.extend(work.iter().map(|v| {
            let bits = (v * scale).to_bits();
            f32::from_bits((bits + ((bits >> 16) & 1) + 0x7FFF) & 0xFFFF_0000)
        }));
    }
    assert_close(&got, &want, 5e-3, "hadamard");
}

#[test]
fn act_quant_fp8_matches_the_cpu_simulation() {
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (row_stride, span, block, n_rows) = (512usize, 448usize, 64usize, 3usize);
    let values = pattern(row_stride * n_rows, 89);
    let buf = upload_f16(dev.as_ref(), &values);

    kernels
        .act_quant_fp8_f16(&buf, row_stride, 0, span, block, n_rows, &stream)
        .unwrap();
    stream.synchronize().unwrap();
    let got = download_f16(dev.as_ref(), &buf, values.len());

    let mut want: Vec<f32> = values.iter().map(|v| f16::from_f32(*v).to_f32()).collect();
    for row in 0..n_rows {
        for group in 0..span / block {
            let at = row * row_stride + group * block;
            let slice = &mut want[at..at + block];
            let amax = slice.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-4);
            let scale = (amax / 448.0).log2().ceil().exp2();
            for v in slice.iter_mut() {
                *v = round_e4m3(*v / scale) * scale;
            }
        }
    }
    assert_close(&got, &want, 5e-3, "kwantyzacja FP8");

    // Ogon poza `span` (wymiary rope) musi zostać nietknięty.
    for row in 0..n_rows {
        for col in span..row_stride {
            let at = row * row_stride + col;
            assert_eq!(
                got[at],
                f16::from_f32(values[at]).to_f32(),
                "kwantyzacja ruszyła wymiar {col} spoza swojego wycinka"
            );
        }
    }
}

#[test]
fn act_quant_fp4_matches_the_cpu_simulation() {
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (width, block, n_rows) = (128usize, 32usize, 3usize);
    let values = pattern(width * n_rows, 101);
    let buf = upload_f16(dev.as_ref(), &values);

    kernels
        .act_quant_fp4_f16(&buf, width, width, block, n_rows, &stream)
        .unwrap();
    stream.synchronize().unwrap();
    let got = download_f16(dev.as_ref(), &buf, values.len());

    const CODEBOOK: [f32; 8] = [0.0, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0, 6.0];
    let mut want: Vec<f32> = values.iter().map(|v| f16::from_f32(*v).to_f32()).collect();
    for chunk in want.chunks_mut(block) {
        let amax = chunk
            .iter()
            .fold(0f32, |m, v| m.max(v.abs()))
            .max(6.0 * (2f32).powi(-126));
        let scale = (amax / 6.0).log2().ceil().exp2();
        for v in chunk.iter_mut() {
            let scaled = (*v / scale).clamp(-6.0, 6.0);
            let sign = if scaled < 0.0 { -1.0 } else { 1.0 };
            let mag = scaled.abs();
            let nearest = CODEBOOK
                .iter()
                .copied()
                .min_by(|a, b| (a - mag).abs().total_cmp(&(b - mag).abs()))
                .unwrap();
            *v = sign * nearest * scale;
        }
    }
    assert_close(&got, &want, 5e-3, "kwantyzacja FP4");
}

/// Najbliższa wartość reprezentowalna w E4M3, z nasyceniem.
fn round_e4m3(x: f32) -> f32 {
    let clamped = x.clamp(-448.0, 448.0);
    let sign = if clamped < 0.0 { -1.0 } else { 1.0 };
    let v = clamped.abs();
    if v < 0.0009765625 {
        return 0.0;
    }
    let exponent = v.log2().floor().max(-6.0);
    let step = (exponent - 3.0).exp2();
    let q = (v / step).round() * step;
    sign * q.min(448.0)
}

fn upload_i32(dev: &dyn Device, values: &[i32]) -> DevBuffer {
    let bytes = unsafe { std::slice::from_raw_parts(values.as_ptr() as *const u8, values.len() * 4) };
    let buf = dev
        .alloc(bytes.len(), MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

fn upload_f32(dev: &dyn Device, values: &[f32]) -> DevBuffer {
    let bytes = unsafe { std::slice::from_raw_parts(values.as_ptr() as *const u8, values.len() * 4) };
    let buf = dev
        .alloc(bytes.len(), MemKind::Device, Pool::Activations)
        .unwrap();
    dev.write(bytes, &buf, 0).unwrap();
    buf
}

/// Pooling kompresora. Tablica slotów niesie logikę okien z zakładką, więc test
/// używa układu, w którym pierwszy blok ma pozycje puste (`-1`) — dokładnie jak
/// blok zerowy przy stopniu kompresji 4, który nie ma poprzednika.
#[test]
fn compressor_pool_matches_the_cpu_reference() {
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (head_dim, ratio, n_blocks) = (64usize, 4usize, 3usize);
    let window = 2 * ratio;
    let n_rows = n_blocks * ratio;

    let kv = pattern(n_rows * head_dim, 43);
    let score = pattern(n_rows * head_dim, 67);
    // Blok 0 nie ma poprzednika: pierwsze `ratio` pozycji jest puste.
    let mut slots = vec![-1i32; n_blocks * window];
    for block in 0..n_blocks {
        for w in 0..window {
            slots[block * window + w] = if w < ratio {
                if block == 0 {
                    -1
                } else {
                    ((block - 1) * ratio + w) as i32
                }
            } else {
                (block * ratio + w - ratio) as i32
            };
        }
    }

    let kv_buf = upload_f16(dev.as_ref(), &kv);
    let sc_buf = upload_f16(dev.as_ref(), &score);
    let slot_buf = upload_i32(dev.as_ref(), &slots);
    let out = dev
        .alloc(n_blocks * head_dim * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    kernels
        .compressor_pool_f16(&out, &kv_buf, &sc_buf, &slot_buf, head_dim, window, n_blocks, &stream)
        .unwrap();
    stream.synchronize().unwrap();
    let got = download_f16(dev.as_ref(), &out, n_blocks * head_dim);

    let r = |v: f32| f16::from_f32(v).to_f32();
    let mut want = Vec::with_capacity(n_blocks * head_dim);
    for block in 0..n_blocks {
        for dim in 0..head_dim {
            let mut max = f32::NEG_INFINITY;
            for w in 0..window {
                let row = slots[block * window + w];
                if row >= 0 {
                    max = max.max(r(score[row as usize * head_dim + dim]));
                }
            }
            let mut denom = 0f32;
            let mut acc = 0f32;
            for w in 0..window {
                let row = slots[block * window + w];
                if row >= 0 {
                    let e = (r(score[row as usize * head_dim + dim]) - max).exp();
                    denom += e;
                    acc += e * r(kv[row as usize * head_dim + dim]);
                }
            }
            want.push(acc / denom);
        }
    }
    assert_close(&got, &want, 5e-3, "pooling kompresora");
}

/// Rzadka uwaga. Test sprawdza też, że kotwica wchodzi WYŁĄCZNIE do mianownika:
/// drugi przebieg z bardzo dużą kotwicą musi wygasić wyjście, a nie je przesunąć.
#[test]
fn sparse_attention_matches_the_cpu_reference() {
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (head_dim, n_heads, n_kv, n_idx) = (128usize, 4usize, 16usize, 10usize);

    let q = pattern(n_heads * head_dim, 31);
    let kv = pattern(n_kv * head_dim, 47);
    // Część indeksów zamaskowana — muszą zostać pominięte, a nie odczytane.
    let idxs: Vec<i32> = (0..n_idx)
        .map(|i| if i % 4 == 3 { -1 } else { ((i * 3) % n_kv) as i32 })
        .collect();

    let q_buf = upload_f16(dev.as_ref(), &q);
    let kv_buf = upload_f16(dev.as_ref(), &kv);
    let idx_buf = upload_i32(dev.as_ref(), &idxs);
    let scale = (head_dim as f32).sqrt().recip();

    for sink_value in [0.5f32, 40.0] {
        let sink: Vec<f32> = (0..n_heads).map(|h| sink_value + h as f32 * 0.1).collect();
        let sink_buf = upload_f32(dev.as_ref(), &sink);
        let out = dev
            .alloc(n_heads * head_dim * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        kernels
            .sparse_attn_f16(
                &out, &q_buf, &kv_buf, &sink_buf, &idx_buf, head_dim, n_heads, n_idx, scale,
                &stream,
            )
            .unwrap();
        stream.synchronize().unwrap();
        let got = download_f16(dev.as_ref(), &out, n_heads * head_dim);

        let r = |v: f32| f16::from_f32(v).to_f32();
        let mut want = vec![0f32; n_heads * head_dim];
        for head in 0..n_heads {
            let valid: Vec<usize> = idxs.iter().filter(|i| **i >= 0).map(|i| *i as usize).collect();
            let scores: Vec<f32> = valid
                .iter()
                .map(|k| {
                    (0..head_dim)
                        .map(|d| r(q[head * head_dim + d]) * r(kv[k * head_dim + d]))
                        .sum::<f32>()
                        * scale
                })
                .collect();
            let max = scores.iter().fold(f32::NEG_INFINITY, |m, v| m.max(*v));
            let exps: Vec<f32> = scores.iter().map(|s| (s - max).exp()).collect();
            let denom: f32 = exps.iter().sum::<f32>() + (sink[head] - max).exp();
            for (w, k) in exps.iter().zip(&valid) {
                for d in 0..head_dim {
                    want[head * head_dim + d] += w * r(kv[k * head_dim + d]);
                }
            }
            for d in 0..head_dim {
                want[head * head_dim + d] /= denom;
            }
        }
        if sink_value < 1.0 {
            assert_close(&got, &want, 1e-2, "rzadka uwaga");
        } else {
            // Kotwica dominuje mianownik, więc wyjście musi zdążyć do zera —
            // gdyby wchodziła też do licznika, zostałoby duże.
            let peak = got.iter().fold(0f32, |m, v| m.max(v.abs()));
            assert!(peak < 1e-2, "kotwica nie wygasiła wyjścia (szczyt {peak:.3e})");
        }
    }
}

fn download_f32(dev: &dyn Device, buf: &DevBuffer, n: usize) -> Vec<f32> {
    let mut bytes = vec![0u8; n * 4];
    dev.read(buf, 0, &mut bytes).unwrap();
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Sinkhorn. Test sprawdza nie tylko zgodność z referencją, ale i własność, dla
/// której ta procedura w ogóle istnieje: macierz ma być podwójnie stochastyczna.
#[test]
fn hc_sinkhorn_matches_the_cpu_reference() {
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (hc, iters, eps, n_tokens) = (4usize, 20usize, 1e-6f32, 5usize);
    let mix_hc = (2 + hc) * hc;

    let mixes = pattern(n_tokens * mix_hc, 59);
    let scale = vec![0.7f32, 1.3, 0.9];
    let base = pattern(mix_hc, 23);

    let mixes_buf = upload_f32(dev.as_ref(), &mixes);
    let scale_buf = upload_f32(dev.as_ref(), &scale);
    let base_buf = upload_f32(dev.as_ref(), &base);
    let pre = dev
        .alloc(n_tokens * hc * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let post = dev
        .alloc(n_tokens * hc * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let comb = dev
        .alloc(n_tokens * hc * hc * 4, MemKind::Device, Pool::Activations)
        .unwrap();

    kernels
        .hc_sinkhorn_f32(
            &pre, &post, &comb, &mixes_buf, &scale_buf, &base_buf, hc, iters, eps, n_tokens,
            &stream,
        )
        .unwrap();
    stream.synchronize().unwrap();
    let got_pre = download_f32(dev.as_ref(), &pre, n_tokens * hc);
    let got_post = download_f32(dev.as_ref(), &post, n_tokens * hc);
    let got_comb = download_f32(dev.as_ref(), &comb, n_tokens * hc * hc);

    let sigmoid = |v: f32| 1.0 / (1.0 + (-v).exp());
    for token in 0..n_tokens {
        let m = &mixes[token * mix_hc..(token + 1) * mix_hc];
        for j in 0..hc {
            let want_pre = sigmoid(m[j] * scale[0] + base[j]) + eps;
            let want_post = 2.0 * sigmoid(m[j + hc] * scale[1] + base[j + hc]);
            assert!((got_pre[token * hc + j] - want_pre).abs() < 1e-5);
            assert!((got_post[token * hc + j] - want_post).abs() < 1e-5);
        }

        let mut want = vec![0f32; hc * hc];
        for j in 0..hc {
            for k in 0..hc {
                let at = j * hc + k + 2 * hc;
                want[j * hc + k] = m[at] * scale[2] + base[at];
            }
        }
        for j in 0..hc {
            let row = &mut want[j * hc..(j + 1) * hc];
            let max = row.iter().fold(f32::NEG_INFINITY, |a, v| a.max(*v));
            let mut sum = 0f32;
            row.iter_mut().for_each(|v| {
                *v = (*v - max).exp();
                sum += *v;
            });
            row.iter_mut().for_each(|v| *v = *v / sum + eps);
        }
        let norm_cols = |w: &mut Vec<f32>| {
            for k in 0..hc {
                let sum: f32 = (0..hc).map(|j| w[j * hc + k]).sum();
                for j in 0..hc {
                    w[j * hc + k] /= sum + eps;
                }
            }
        };
        norm_cols(&mut want);
        for _ in 0..iters - 1 {
            for j in 0..hc {
                let sum: f32 = want[j * hc..(j + 1) * hc].iter().sum();
                want[j * hc..(j + 1) * hc].iter_mut().for_each(|v| *v /= sum + eps);
            }
            norm_cols(&mut want);
        }
        for i in 0..hc * hc {
            let g = got_comb[token * hc * hc + i];
            assert!(
                (g - want[i]).abs() < 1e-5,
                "token {token}, element {i}: {g} zamiast {}",
                want[i]
            );
        }

        // Własność docelowa: sumy wierszy i kolumn bliskie jedności.
        for j in 0..hc {
            let row: f32 = (0..hc).map(|k| got_comb[token * hc * hc + j * hc + k]).sum();
            let col: f32 = (0..hc).map(|k| got_comb[token * hc * hc + k * hc + j]).sum();
            assert!(
                (row - 1.0).abs() < 1e-2 && (col - 1.0).abs() < 1e-2,
                "macierz nie jest podwójnie stochastyczna: wiersz {row}, kolumna {col}"
            );
        }
    }
}

/// Redukcja i rozprowadzenie kopii HC. Rozprowadzenie sprawdza też kierunek
/// indeksowania macierzy mieszającej — transpozycja daje poprawny kształt.
#[test]
fn hc_reduce_and_expand_match_the_cpu_reference() {
    let dev = device();
    let kernels = Kernels::load(dev.clone()).unwrap();
    let stream = dev.create_stream().unwrap();
    let (dim, hc, n_tokens) = (96usize, 4usize, 3usize);

    let x = pattern(n_tokens * hc * dim, 61);
    let block_out = pattern(n_tokens * dim, 73);
    let pre = pattern(n_tokens * hc, 17).iter().map(|v| v.abs() + 0.1).collect::<Vec<f32>>();
    let post = pattern(n_tokens * hc, 19).iter().map(|v| v.abs() + 0.1).collect::<Vec<f32>>();
    // Macierz niesymetryczna, żeby transpozycja była wykrywalna.
    let comb: Vec<f32> = (0..n_tokens * hc * hc)
        .map(|i| ((i * 13 % 7) as f32 + 1.0) * 0.11)
        .collect();

    let x_buf = upload_f16(dev.as_ref(), &x);
    let blk_buf = upload_f16(dev.as_ref(), &block_out);
    let pre_buf = upload_f32(dev.as_ref(), &pre);
    let post_buf = upload_f32(dev.as_ref(), &post);
    let comb_buf = upload_f32(dev.as_ref(), &comb);

    let reduced = dev
        .alloc(n_tokens * dim * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    kernels
        .hc_reduce_f16(&reduced, &x_buf, &pre_buf, dim, hc, n_tokens, &stream)
        .unwrap();
    stream.synchronize().unwrap();
    let got_reduced = download_f16(dev.as_ref(), &reduced, n_tokens * dim);

    let r = |v: f32| f16::from_f32(v).to_f32();
    let mut want_reduced = Vec::with_capacity(n_tokens * dim);
    for token in 0..n_tokens {
        for d in 0..dim {
            let mut acc = 0f32;
            for copy in 0..hc {
                acc += pre[token * hc + copy] * r(x[(token * hc + copy) * dim + d]);
            }
            want_reduced.push(acc);
        }
    }
    assert_close(&got_reduced, &want_reduced, 5e-3, "redukcja HC");

    let expanded = dev
        .alloc(n_tokens * hc * dim * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    kernels
        .hc_expand_f16(
            &expanded, &blk_buf, &x_buf, &post_buf, &comb_buf, dim, hc, n_tokens, &stream,
        )
        .unwrap();
    stream.synchronize().unwrap();
    let got_expanded = download_f16(dev.as_ref(), &expanded, n_tokens * hc * dim);

    let mut want_expanded = vec![0f32; n_tokens * hc * dim];
    for token in 0..n_tokens {
        for out_copy in 0..hc {
            for d in 0..dim {
                let mut acc = post[token * hc + out_copy] * r(block_out[token * dim + d]);
                for in_copy in 0..hc {
                    acc += comb[token * hc * hc + in_copy * hc + out_copy]
                        * r(x[(token * hc + in_copy) * dim + d]);
                }
                want_expanded[(token * hc + out_copy) * dim + d] = acc;
            }
        }
    }
    assert_close(&got_expanded, &want_expanded, 5e-3, "rozprowadzenie HC");
}
