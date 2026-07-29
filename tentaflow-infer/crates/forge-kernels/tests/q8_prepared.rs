// =============================================================================
// Plik: q8_prepared.rs
// Opis: Sprawdza współdzieloną kwantyzację Q8_1 dla grupy projekcji DeltaNet.
// Przykład: cargo test -p forge-kernels --release --test q8_prepared -- --nocapture
// =============================================================================

use std::sync::Arc;

use forge_hal::{PoolSizes, gpu};
use forge_hal::{Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

fn device() -> Option<Arc<dyn Device>> {
    match gpu::open(
        0,
        PoolSizes {
            weights: 256 << 20,
            kv_cache: 4 << 20,
            activations: 128 << 20,
            kv_page_size: 256 << 10,
        },
    ) {
        Ok(device) => Some(device),
        Err(error) => {
            eprintln!("pominięto test prepared Q8 bez CUDA: {error}");
            None
        }
    }
}

fn weights(rows: usize, cols: usize, seed: usize) -> Vec<u8> {
    let mut data = vec![0u8; rows * (cols / 32) * 34];
    for (index, block) in data.chunks_exact_mut(34).enumerate() {
        block[..2].copy_from_slice(&f16::from_f32(0.0078125).to_bits().to_le_bytes());
        for (byte, value) in block[2..].iter_mut().enumerate() {
            *value = ((index * 17 + byte * 13 + seed * 29) & 0xff) as u8;
        }
    }
    data
}

fn activations(tokens: usize, cols: usize) -> Vec<f16> {
    (0..tokens * cols)
        .map(|index| f16::from_f32(((index * 31 % 67) as f32 - 33.0) / 16.0))
        .collect()
}

fn argmax(values: &[u8]) -> usize {
    values
        .chunks_exact(2)
        .enumerate()
        .max_by(|(_, left), (_, right)| {
            f16::from_bits(u16::from_le_bytes([left[0], left[1]]))
                .to_f32()
                .total_cmp(&f16::from_bits(u16::from_le_bytes([right[0], right[1]])).to_f32())
        })
        .map(|(index, _)| index)
        .unwrap()
}

fn assert_top1_per_token(actual: &[u8], expected: &[u8], tokens: usize, rows: usize, label: &str) {
    let row_bytes = rows * 2;
    for token in 0..tokens {
        let start = token * row_bytes;
        let end = start + row_bytes;
        assert_eq!(
            argmax(&actual[start..end]),
            argmax(&expected[start..end]),
            "top-1 {label}, token={token}, T={tokens}"
        );
    }
}

#[test]
fn prepared_q8_zachowuje_bity_canary_i_top1_dla_t6_i_t8() {
    let Some(device) = device() else { return };
    let kernels = Kernels::load(device.clone()).unwrap();
    let stream = device.create_stream().unwrap();
    let cols = 512usize;
    let rows = [37usize, 19, 7];
    let out_cols = 640usize;
    let out_rows = 23usize;
    let guard = 23usize;
    let weight_offset = 68usize;
    let mut weight_buffers = Vec::with_capacity(rows.len());
    for (seed, &projection_rows) in rows.iter().enumerate() {
        let mut host = vec![0xa5; weight_offset];
        host.extend(weights(projection_rows, cols, seed));
        let buffer = device
            .alloc(host.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        device.write(&host, &buffer, 0).unwrap();
        weight_buffers.push(buffer);
    }
    let mut out_host_weights = vec![0xa5; weight_offset];
    out_host_weights.extend(weights(out_rows, out_cols, rows.len()));
    let out_weights = device
        .alloc(out_host_weights.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    device.write(&out_host_weights, &out_weights, 0).unwrap();

    for tokens in [6usize, 8] {
        let host_x = activations(tokens, cols);
        let x = device
            .alloc(host_x.len() * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        device
            .write(bytemuck::cast_slice::<f16, u8>(&host_x), &x, 0)
            .unwrap();
        let mut baseline_outputs = Vec::with_capacity(rows.len());
        let mut baseline_initials = Vec::with_capacity(rows.len());
        for (&projection_rows, weight) in rows.iter().zip(&weight_buffers) {
            let initial = vec![f16::from_bits(0x7bcd); tokens * projection_rows + guard];
            let output = device
                .alloc(initial.len() * 2, MemKind::Device, Pool::Activations)
                .unwrap();
            device
                .write(bytemuck::cast_slice::<f16, u8>(&initial), &output, 0)
                .unwrap();
            kernels
                .gemm_q8_0_i8mma_at(
                    &output,
                    weight,
                    weight_offset,
                    &x,
                    projection_rows,
                    cols,
                    tokens,
                    &stream,
                )
                .unwrap();
            baseline_outputs.push(output);
            baseline_initials.push(initial);
        }
        stream.synchronize().unwrap();
        let mut baseline_bytes = Vec::with_capacity(rows.len());
        for (output, initial) in baseline_outputs.iter().zip(&baseline_initials) {
            let mut bytes = vec![0u8; initial.len() * 2];
            device.read(output, 0, &mut bytes).unwrap();
            baseline_bytes.push(bytes);
        }
        let host_normed = activations(tokens, out_cols);
        let normed = device
            .alloc(host_normed.len() * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        device
            .write(bytemuck::cast_slice::<f16, u8>(&host_normed), &normed, 0)
            .unwrap();
        let out_initial = vec![f16::from_bits(0x7bcd); tokens * out_rows + guard];
        let out_baseline = device
            .alloc(out_initial.len() * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        device
            .write(
                bytemuck::cast_slice::<f16, u8>(&out_initial),
                &out_baseline,
                0,
            )
            .unwrap();
        kernels
            .gemm_q8_0_i8mma_at(
                &out_baseline,
                &out_weights,
                weight_offset,
                &normed,
                out_rows,
                out_cols,
                tokens,
                &stream,
            )
            .unwrap();
        device.synchronize().unwrap();
        let mut out_baseline_bytes = vec![0u8; out_initial.len() * 2];
        device
            .read(&out_baseline, 0, &mut out_baseline_bytes)
            .unwrap();

        let mut prepared = kernels.prepare_q8_1(&x, cols, tokens, &stream).unwrap();
        let mut prepared_outputs = Vec::with_capacity(rows.len());
        let mut prepared_initials = Vec::with_capacity(rows.len());
        for (&projection_rows, weight) in rows.iter().zip(&weight_buffers) {
            let initial = vec![f16::from_bits(0x7bcd); tokens * projection_rows + guard];
            let output = device
                .alloc(initial.len() * 2, MemKind::Device, Pool::Activations)
                .unwrap();
            device
                .write(bytemuck::cast_slice::<f16, u8>(&initial), &output, 0)
                .unwrap();
            kernels
                .gemm_q8_0_i8mma_prepared_at(
                    &output,
                    weight,
                    weight_offset,
                    &mut prepared,
                    projection_rows,
                    cols,
                    tokens,
                )
                .unwrap();
            prepared_outputs.push(output);
            prepared_initials.push(initial);
        }
        stream.synchronize().unwrap();
        for (index, ((&projection_rows, output), initial)) in rows
            .iter()
            .zip(&prepared_outputs)
            .zip(&prepared_initials)
            .enumerate()
        {
            let mut bytes = vec![0u8; initial.len() * 2];
            device.read(output, 0, &mut bytes).unwrap();
            let output_bytes = tokens * projection_rows * 2;
            assert_eq!(
                &bytes[..output_bytes],
                &baseline_bytes[index][..output_bytes],
                "bity projekcji {index}, T={tokens}"
            );
            assert_eq!(
                &bytes[output_bytes..],
                bytemuck::cast_slice::<f16, u8>(&initial[tokens * projection_rows..]),
                "canary projekcji {index}, T={tokens}"
            );
            assert_top1_per_token(
                &bytes[..output_bytes],
                &baseline_bytes[index][..output_bytes],
                tokens,
                projection_rows,
                &format!("projekcji {index}"),
            );
        }
        let out_probe = device
            .alloc(out_initial.len() * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        assert!(kernels
            .gemm_q8_0_i8mma_prepared_at(
                &out_probe,
                &out_weights,
                weight_offset,
                &mut prepared,
                out_rows,
                out_cols,
                tokens,
            )
            .is_err());
        drop(prepared);

        let out_oracle = device
            .alloc(out_initial.len() * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        device
            .write(
                bytemuck::cast_slice::<f16, u8>(&out_initial),
                &out_oracle,
                0,
            )
            .unwrap();
        kernels
            .gemm_q8_0_i8mma_at(
                &out_oracle,
                &out_weights,
                weight_offset,
                &normed,
                out_rows,
                out_cols,
                tokens,
                &stream,
            )
            .unwrap();
        device.synchronize().unwrap();
        let mut out_oracle_bytes = vec![0u8; out_initial.len() * 2];
        device.read(&out_oracle, 0, &mut out_oracle_bytes).unwrap();
        let out_bytes = tokens * out_rows * 2;
        assert_eq!(
            &out_oracle_bytes[..out_bytes],
            &out_baseline_bytes[..out_bytes],
            "bity osobnej projekcji out, T={tokens}"
        );
        assert_eq!(
            &out_oracle_bytes[out_bytes..],
            bytemuck::cast_slice::<f16, u8>(&out_initial[tokens * out_rows..]),
            "canary osobnej projekcji out, T={tokens}"
        );
        assert_top1_per_token(
            &out_oracle_bytes[..out_bytes],
            &out_baseline_bytes[..out_bytes],
            tokens,
            out_rows,
            "osobnej projekcji out",
        );
    }
}

#[test]
fn prepared_q8_chroni_scratch_miedzy_dwoma_streamami() {
    let Some(device) = device() else { return };
    let kernels = Kernels::load(device.clone()).unwrap();
    let stream_a = device.create_stream().unwrap();
    let stream_b = device.create_stream().unwrap();
    let tokens_a = 6usize;
    let tokens_b = 8usize;
    let cols = 512usize;
    let rows = [2048usize, 48, 48];
    let weight_offset = 68usize;
    let guard = 19usize;
    let mut weight_buffers = Vec::with_capacity(rows.len());
    for (seed, &projection_rows) in rows.iter().enumerate() {
        let mut host = vec![0xa5; weight_offset];
        host.extend(weights(projection_rows, cols, seed));
        let buffer = device
            .alloc(host.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        device.write(&host, &buffer, 0).unwrap();
        weight_buffers.push(buffer);
    }
    let host_a = activations(tokens_a, cols);
    let host_b = activations(tokens_b, cols)
        .iter()
        .enumerate()
        .map(|(index, value)| f16::from_f32(value.to_f32() * -0.75 + (index % 11) as f32 / 32.0))
        .collect::<Vec<_>>();
    let x_a = device
        .alloc(host_a.len() * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let x_b = device
        .alloc(host_b.len() * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    device
        .write(bytemuck::cast_slice::<f16, u8>(&host_a), &x_a, 0)
        .unwrap();
    device
        .write(bytemuck::cast_slice::<f16, u8>(&host_b), &x_b, 0)
        .unwrap();

    let baseline = |x: &forge_hal::DevBuffer, tokens: usize, stream: &forge_hal::Stream| {
        let mut outputs = Vec::with_capacity(rows.len());
        for (&projection_rows, weight) in rows.iter().zip(&weight_buffers) {
            let initial = vec![f16::from_bits(0x7bcd); tokens * projection_rows + guard];
            let output = device
                .alloc(initial.len() * 2, MemKind::Device, Pool::Activations)
                .unwrap();
            device
                .write(bytemuck::cast_slice::<f16, u8>(&initial), &output, 0)
                .unwrap();
            kernels
                .gemm_q8_0_i8mma_at(
                    &output,
                    weight,
                    weight_offset,
                    x,
                    projection_rows,
                    cols,
                    tokens,
                    stream,
                )
                .unwrap();
            outputs.push(output);
        }
        stream.synchronize().unwrap();
        outputs
            .iter()
            .zip(rows)
            .map(|(output, projection_rows)| {
                let mut bytes = vec![0u8; (tokens * projection_rows + guard) * 2];
                device.read(output, 0, &mut bytes).unwrap();
                bytes
            })
            .collect::<Vec<_>>()
    };
    let expected_a = baseline(&x_a, tokens_a, &stream_a);
    let expected_b = baseline(&x_b, tokens_b, &stream_b);

    let mut outputs_a = Vec::with_capacity(rows.len());
    {
        let mut prepared = kernels
            .prepare_q8_1(&x_a, cols, tokens_a, &stream_a)
            .unwrap();
        for (&projection_rows, weight) in rows.iter().zip(&weight_buffers) {
            let initial = vec![f16::from_bits(0x7bcd); tokens_a * projection_rows + guard];
            let output = device
                .alloc(initial.len() * 2, MemKind::Device, Pool::Activations)
                .unwrap();
            device
                .write(bytemuck::cast_slice::<f16, u8>(&initial), &output, 0)
                .unwrap();
            kernels
                .gemm_q8_0_i8mma_prepared_at(
                    &output,
                    weight,
                    weight_offset,
                    &mut prepared,
                    projection_rows,
                    cols,
                    tokens_a,
                )
                .unwrap();
            outputs_a.push(output);
        }
    }
    let mut outputs_b = Vec::with_capacity(rows.len());
    std::thread::scope(|scope| {
        let churn_device = device.clone();
        let churn = scope.spawn(move || {
            for _ in 0..128 {
                let codes = churn_device
                    .alloc(tokens_b * cols, MemKind::Device, Pool::Activations)
                    .unwrap();
                let scales = churn_device
                    .alloc(
                        tokens_b * (cols / 32) * 4,
                        MemKind::Device,
                        Pool::Activations,
                    )
                    .unwrap();
                drop((codes, scales));
            }
        });
        {
            let mut prepared = kernels
                .prepare_q8_1(&x_b, cols, tokens_b, &stream_b)
                .unwrap();
            for (&projection_rows, weight) in rows.iter().zip(&weight_buffers) {
                let initial = vec![f16::from_bits(0x7bcd); tokens_b * projection_rows + guard];
                let output = device
                    .alloc(initial.len() * 2, MemKind::Device, Pool::Activations)
                    .unwrap();
                device
                    .write(bytemuck::cast_slice::<f16, u8>(&initial), &output, 0)
                    .unwrap();
                kernels
                    .gemm_q8_0_i8mma_prepared_at(
                        &output,
                        weight,
                        weight_offset,
                        &mut prepared,
                        projection_rows,
                        cols,
                        tokens_b,
                    )
                    .unwrap();
                outputs_b.push(output);
            }
        }
        churn.join().unwrap();
    });
    stream_b.synchronize().unwrap();

    for (lane, tokens, outputs, expected) in [
        (0usize, tokens_a, &outputs_a, &expected_a),
        (1usize, tokens_b, &outputs_b, &expected_b),
    ]
    .into_iter()
    {
        for (projection, ((output, expected), projection_rows)) in
            outputs.iter().zip(expected).zip(rows).enumerate()
        {
            let mut actual = vec![0u8; (tokens * projection_rows + guard) * 2];
            device.read(output, 0, &mut actual).unwrap();
            assert_eq!(
                &actual, expected,
                "lane={lane}, T={tokens}, projekcja={projection}"
            );
        }
    }
}

fn measure(
    device: &dyn Device,
    stream: &forge_hal::Stream,
    repetitions: usize,
    mut launch: impl FnMut(),
) -> f64 {
    for _ in 0..8 {
        launch();
    }
    device.synchronize().unwrap();
    let start = device.create_timing_event().unwrap();
    let end = device.create_timing_event().unwrap();
    device.record_event(&start, stream).unwrap();
    for _ in 0..repetitions {
        launch();
    }
    device.record_event(&end, stream).unwrap();
    end.synchronize().unwrap();
    f64::from(device.elapsed_event_ms(&start, &end).unwrap().unwrap()) * 1000.0 / repetitions as f64
}

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

#[test]
#[ignore]
fn bench_prepared_q8_grupa_deltanet() {
    let Some(device) = device() else { return };
    let kernels = Kernels::load(device.clone()).unwrap();
    let stream = device.create_stream().unwrap();
    let cols = 5120usize;
    let rows = [5120usize, 48, 48];
    let output_rows = 5120usize;
    let mut weight_buffers = Vec::with_capacity(rows.len());
    let mut outputs = Vec::with_capacity(rows.len());
    for (seed, &projection_rows) in rows.iter().enumerate() {
        let host = weights(projection_rows, cols, seed);
        let weight = device
            .alloc(host.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        device.write(&host, &weight, 0).unwrap();
        weight_buffers.push(weight);
        outputs.push(
            device
                .alloc(8 * projection_rows * 2, MemKind::Device, Pool::Activations)
                .unwrap(),
        );
    }
    let output_host_weights = weights(output_rows, 6144, rows.len());
    let output_weights = device
        .alloc(output_host_weights.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    device
        .write(&output_host_weights, &output_weights, 0)
        .unwrap();
    let output_projection = device
        .alloc(8 * output_rows * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let output_oracle = device
        .alloc(8 * output_rows * 2, MemKind::Device, Pool::Activations)
        .unwrap();

    for tokens in [6usize, 8] {
        let host_x = activations(tokens, cols);
        let x = device
            .alloc(host_x.len() * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        device
            .write(bytemuck::cast_slice::<f16, u8>(&host_x), &x, 0)
            .unwrap();
        let host_output_x = activations(tokens, 6144);
        let output_x = device
            .alloc(host_output_x.len() * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        device
            .write(
                bytemuck::cast_slice::<f16, u8>(&host_output_x),
                &output_x,
                0,
            )
            .unwrap();
        kernels
            .gemm_q8_0_i8mma_at(
                &output_projection,
                &output_weights,
                0,
                &output_x,
                output_rows,
                6144,
                tokens,
                &stream,
            )
            .unwrap();
        device.synchronize().unwrap();
        let mut output_baseline_bytes = vec![0u8; tokens * output_rows * 2];
        device
            .read(&output_projection, 0, &mut output_baseline_bytes)
            .unwrap();
        let baseline_launch = || {
            for ((output, weight), &projection_rows) in
                outputs.iter().zip(&weight_buffers).zip(&rows)
            {
                kernels
                    .gemm_q8_0_i8mma_at(
                        output,
                        weight,
                        0,
                        &x,
                        projection_rows,
                        cols,
                        tokens,
                        &stream,
                    )
                    .unwrap();
            }
        };
        let shared_launch = || {
            let mut prepared = kernels.prepare_q8_1(&x, cols, tokens, &stream).unwrap();
            for ((output, weight), &projection_rows) in
                outputs.iter().zip(&weight_buffers).zip(&rows)
            {
                kernels
                    .gemm_q8_0_i8mma_prepared_at(
                        output,
                        weight,
                        0,
                        &mut prepared,
                        projection_rows,
                        cols,
                        tokens,
                    )
                    .unwrap();
            }
        };
        let mut baseline_samples = Vec::with_capacity(7);
        let mut shared_samples = Vec::with_capacity(7);
        for trial in 0..7 {
            if trial % 2 == 0 {
                baseline_samples.push(measure(device.as_ref(), &stream, 200, baseline_launch));
                shared_samples.push(measure(device.as_ref(), &stream, 200, shared_launch));
            } else {
                shared_samples.push(measure(device.as_ref(), &stream, 200, shared_launch));
                baseline_samples.push(measure(device.as_ref(), &stream, 200, baseline_launch));
            }
        }
        let baseline = median(baseline_samples);
        let shared = median(shared_samples);
        kernels
            .gemm_q8_0_i8mma_at(
                &output_oracle,
                &output_weights,
                0,
                &output_x,
                output_rows,
                6144,
                tokens,
                &stream,
            )
            .unwrap();
        device.synchronize().unwrap();
        let mut output_oracle_bytes = vec![0u8; tokens * output_rows * 2];
        device
            .read(&output_oracle, 0, &mut output_oracle_bytes)
            .unwrap();
        assert_eq!(output_oracle_bytes, output_baseline_bytes);
        assert_top1_per_token(
            &output_oracle_bytes,
            &output_baseline_bytes,
            tokens,
            output_rows,
            "real-shape out oracle",
        );
        println!(
            "Q8 DeltaNet T={tokens}: grupa baseline={baseline:.3} us (6 launchy), shared={shared:.3} us (4 launchy), zmiana={:.2}%; out oracle osobno (2 launchy)",
            100.0 * (baseline - shared) / baseline
        );
    }
}
