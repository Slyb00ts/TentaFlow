// =============================================================================
// Plik: mtp_segmented.rs
// Opis: Sprawdza segmentowane decyzje acceptance/correction MTP B2 na GPU.
// Przykład: cargo test -p forge-kernels --test mtp_segmented -- --nocapture
// =============================================================================

use std::sync::Arc;

use forge_hal::cuda::{CudaDevice, PoolSizes};
use forge_hal::{Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

fn device() -> Option<Arc<CudaDevice>> {
    match CudaDevice::new(
        0,
        PoolSizes {
            weights: 16 << 20,
            kv_cache: 4 << 20,
            activations: 16 << 20,
            kv_page_size: 256 << 10,
        },
    ) {
        Ok(device) => Some(device),
        Err(error) => {
            eprintln!("pominięto test MTP B2 bez CUDA: {error}");
            None
        }
    }
}

fn run_case(input: &[i32], predictions: &[i32], t: usize, expected: &[i32]) {
    let Some(device) = device() else { return };
    let kernels = Kernels::load(device.clone()).unwrap();
    let stream = device.create_stream().unwrap();
    let input_buffer = device
        .alloc(input.len() * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let prediction_buffer = device
        .alloc(predictions.len() * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let decisions = device
        .alloc(4 * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    device
        .write(bytemuck::cast_slice(input), &input_buffer, 0)
        .unwrap();
    device
        .write(bytemuck::cast_slice(predictions), &prediction_buffer, 0)
        .unwrap();
    kernels
        .mtp_verify_decide_segmented(&decisions, &prediction_buffer, &input_buffer, 2, t, &stream)
        .unwrap();
    device.synchronize().unwrap();
    let mut bytes = vec![0u8; 4 * 4];
    device.read(&decisions, 0, &mut bytes).unwrap();
    assert_eq!(bytemuck::cast_slice::<u8, i32>(&bytes), expected);
}

#[test]
fn decyzje_b2_obejmuja_acceptance_od_zera_do_k_i_token_korekty() {
    run_case(
        &[10, 11, 12, 20, 21, 22],
        &[90, 91, 92, 21, 77, 93],
        3,
        &[1, 90, 2, 77],
    );
    run_case(
        &[10, 11, 12, 13, 20, 21, 22, 23],
        &[11, 12, 13, 44, 21, 22, 99, 55],
        4,
        &[4, 44, 3, 99],
    );
}

fn run_pack_i_batch_gather_embeddingu(steps: usize) {
    let Some(device) = device() else { return };
    let kernels = Kernels::load(device.clone()).unwrap();
    let stream = device.create_stream().unwrap();
    let total = 2 * steps;
    let hidden = 64usize;
    let vocab = 5usize;
    let guard = 19usize;
    let lane0_values = [3i32, 1, -1, 2, 0];
    let lane1_values = [4i32, 0, vocab as i32, 3, 0];
    let alloc = |bytes| {
        device
            .alloc(bytes, MemKind::Device, Pool::Activations)
            .unwrap()
    };
    let lane0 = alloc(5 * 4);
    let lane1 = alloc(5 * 4);
    let bases = alloc(2 * 4);
    let ids = alloc((total + guard) * 4);
    let positions = alloc((total + guard) * 4);
    let visible = alloc((total + guard) * 4);
    device
        .write(bytemuck::cast_slice(&lane0_values), &lane0, 0)
        .unwrap();
    device
        .write(bytemuck::cast_slice(&lane1_values), &lane1, 0)
        .unwrap();
    device
        .write(bytemuck::cast_slice(&[7i32, 19]), &bases, 0)
        .unwrap();
    for output in [&ids, &positions, &visible] {
        device
            .write(
                bytemuck::cast_slice(&vec![0x6bad_cafeu32; total + guard]),
                output,
                0,
            )
            .unwrap();
    }
    kernels
        .mtp_pack_verify_inputs(
            &ids, &positions, &visible, &lane0, &lane1, &bases, steps, &stream,
        )
        .unwrap();

    let mut f16_weights = Vec::with_capacity(vocab * hidden);
    for row in 0..vocab {
        f16_weights.extend(std::iter::repeat_n(f16::from_f32((row + 1) as f32), hidden));
    }
    let mut q8_weights = vec![0u8; vocab * (hidden / 32) * 34];
    for block in q8_weights.chunks_exact_mut(34) {
        block[..2].copy_from_slice(&f16::ONE.to_bits().to_le_bytes());
        block[2..].fill(1);
    }
    let nvfp4_weights = vec![0u8; vocab * (hidden / 64) * 36];
    let f16_table = device
        .alloc(f16_weights.len() * 2, MemKind::Device, Pool::Weights)
        .unwrap();
    let q8 = device
        .alloc(q8_weights.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    let nvfp4 = device
        .alloc(nvfp4_weights.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    device
        .write(bytemuck::cast_slice(&f16_weights), &f16_table, 0)
        .unwrap();
    device.write(&q8_weights, &q8, 0).unwrap();
    device.write(&nvfp4_weights, &nvfp4, 0).unwrap();
    let canary = f16::from_bits(0x7bcd);
    let output_values = vec![canary; total * hidden + guard];
    let f16_output = alloc(output_values.len() * 2);
    let q8_output = alloc(output_values.len() * 2);
    let nvfp4_output = alloc(output_values.len() * 2);
    for output in [&f16_output, &q8_output, &nvfp4_output] {
        device
            .write(bytemuck::cast_slice(&output_values), output, 0)
            .unwrap();
    }
    kernels
        .gather_rows_f16(&f16_output, &f16_table, &ids, total, hidden, &stream)
        .unwrap();
    kernels
        .gather_q8_0_rows_f16(&q8_output, &q8, &ids, total, vocab, hidden, &stream)
        .unwrap();
    kernels
        .gather_nvfp4_gguf_rows_f16(
            &nvfp4_output,
            &nvfp4,
            &ids,
            total,
            vocab,
            hidden,
            1.0,
            &stream,
        )
        .unwrap();
    device.synchronize().unwrap();

    let mut expected_ids = lane0_values[..steps].to_vec();
    expected_ids.extend_from_slice(&lane1_values[..steps]);
    let mut expected_positions: Vec<i32> = (7..7 + steps as i32).collect();
    expected_positions.extend(19..19 + steps as i32);
    let expected_visible: Vec<i32> = expected_positions.iter().map(|value| value + 1).collect();
    for (buffer, expected) in [
        (&ids, expected_ids.as_slice()),
        (&positions, expected_positions.as_slice()),
        (&visible, expected_visible.as_slice()),
    ] {
        let mut bytes = vec![0u8; (total + guard) * 4];
        device.read(buffer, 0, &mut bytes).unwrap();
        let values = bytemuck::cast_slice::<u8, i32>(&bytes);
        assert_eq!(&values[..total], expected);
        assert!(values[total..]
            .iter()
            .all(|value| *value as u32 == 0x6bad_cafe));
    }
    let invalid_rows = [2usize, steps + 2];
    for output in [&f16_output, &q8_output, &nvfp4_output] {
        let mut bytes = vec![0u8; output_values.len() * 2];
        device.read(output, 0, &mut bytes).unwrap();
        let values = bytemuck::cast_slice::<u8, f16>(&bytes);
        assert!(values[..total * hidden]
            .iter()
            .all(|value| *value != canary));
        for row in invalid_rows {
            assert!(values[row * hidden..(row + 1) * hidden]
                .iter()
                .all(|value| *value == f16::ZERO));
        }
        assert!(values[total * hidden..]
            .iter()
            .all(|value| *value == canary));
    }
}

#[test]
fn pack_i_batch_gather_embeddingu_zachowuja_canary_dla_k2_i_k3() {
    for steps in [3, 4] {
        run_pack_i_batch_gather_embeddingu(steps);
    }
}

#[test]
fn segmentowana_atencja_uzywa_osobnych_tablic_stron_lane() {
    let Some(device) = device() else { return };
    let kernels = Kernels::load(device.clone()).unwrap();
    let stream = device.create_stream().unwrap();
    let head_dim = 256usize;
    let page_size = 2usize;
    let tokens = 2usize;
    let batch = 2usize;
    let elements = batch * tokens * head_dim;
    let q = device
        .alloc(elements * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let k = device
        .alloc(elements * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let v = device
        .alloc(elements * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let output = device
        .alloc(elements * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let cache_elements = 4 * page_size * head_dim;
    let k_cache = device
        .alloc(cache_elements * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let v_cache = device
        .alloc(cache_elements * 2, MemKind::Device, Pool::Activations)
        .unwrap();
    let page_tables = device
        .alloc(4 * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let base_positions = device
        .alloc(2 * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let visible_lens = device
        .alloc(4 * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let zero = vec![f16::ZERO; elements];
    let mut values = Vec::with_capacity(elements);
    for value in [2.0f32, 6.0, 20.0, 28.0] {
        values.extend(std::iter::repeat_n(f16::from_f32(value), head_dim));
    }
    device.write(bytemuck::cast_slice(&zero), &q, 0).unwrap();
    device.write(bytemuck::cast_slice(&zero), &k, 0).unwrap();
    device.write(bytemuck::cast_slice(&values), &v, 0).unwrap();
    device
        .write(bytemuck::cast_slice(&[0i32, 1, 2, 3]), &page_tables, 0)
        .unwrap();
    device
        .write(bytemuck::cast_slice(&[0i32, 0]), &base_positions, 0)
        .unwrap();
    device
        .write(bytemuck::cast_slice(&[1i32, 2, 1, 2]), &visible_lens, 0)
        .unwrap();
    kernels
        .kv_append_batch_segmented_f16(
            &k_cache,
            &v_cache,
            &k,
            &v,
            &page_tables,
            &base_positions,
            batch,
            tokens,
            2,
            1,
            page_size,
            head_dim,
            &stream,
        )
        .unwrap();
    kernels
        .attn_verify_segmented_f16_hd256(
            &output,
            &q,
            &k_cache,
            &v_cache,
            &page_tables,
            &visible_lens,
            batch,
            tokens,
            1,
            1,
            page_size,
            2,
            1.0,
            &stream,
        )
        .unwrap();
    device.synchronize().unwrap();
    let mut bytes = vec![0u8; elements * 2];
    device.read(&output, 0, &mut bytes).unwrap();
    let actual = bytemuck::cast_slice::<u8, f16>(&bytes);
    for (row, expected) in [2.0f32, 4.0, 20.0, 24.0].into_iter().enumerate() {
        for value in &actual[row * head_dim..(row + 1) * head_dim] {
            assert_eq!(value.to_f32(), expected, "wiersz {row}");
        }
    }
}

#[test]
fn wspoldzielony_skan_i_commit_odtwarzaja_checkpointy_d128() {
    let Some(device) = device() else { return };
    let kernels = Kernels::load(device.clone()).unwrap();
    let stream = device.create_stream().unwrap();
    let batch = 2usize;
    let steps = 4usize;
    let heads = 2usize;
    let state_dim = 128usize;
    let state_elements = heads * state_dim * state_dim;
    let value_elements = batch * steps * heads * state_dim;
    let gate_elements = batch * steps * heads;
    let alloc = |bytes| {
        device
            .alloc(bytes, MemKind::Device, Pool::Activations)
            .unwrap()
    };
    let initial = alloc(batch * state_elements * 4);
    let q = alloc(value_elements * 2);
    let k = alloc(value_elements * 2);
    let v = alloc(value_elements * 2);
    let g = alloc(gate_elements * 4);
    let beta = alloc(gate_elements * 4);
    let old_output = alloc(value_elements * 2);
    let new_output = alloc(value_elements * 2);
    let checkpoints = alloc(batch * steps * state_elements * 4);
    let old_states = alloc(batch * state_elements * 4);
    let new_states = alloc(batch * state_elements * 4);
    let decisions = alloc(batch * 2 * 4);

    let initial_values: Vec<f32> = (0..batch * state_elements)
        .map(|index| (index % 23) as f32 * 0.0001 - 0.001)
        .collect();
    let vectors: Vec<f16> = (0..value_elements)
        .map(|index| f16::from_f32((index % 19) as f32 * 0.002 - 0.018))
        .collect();
    let values: Vec<f16> = (0..value_elements)
        .map(|index| f16::from_f32((index % 17) as f32 * 0.003 - 0.024))
        .collect();
    device
        .write(bytemuck::cast_slice(&initial_values), &initial, 0)
        .unwrap();
    device.write(bytemuck::cast_slice(&vectors), &q, 0).unwrap();
    device.write(bytemuck::cast_slice(&vectors), &k, 0).unwrap();
    device.write(bytemuck::cast_slice(&values), &v, 0).unwrap();
    device
        .write(bytemuck::cast_slice(&vec![-0.04f32; gate_elements]), &g, 0)
        .unwrap();
    device
        .write(
            bytemuck::cast_slice(&vec![0.35f32; gate_elements]),
            &beta,
            0,
        )
        .unwrap();
    device
        .write(bytemuck::cast_slice(&[1i32, 7, 4, 9]), &decisions, 0)
        .unwrap();

    kernels
        .deltanet_gated_scan_segmented_d128_f16(
            &old_output,
            &checkpoints,
            &initial,
            &q,
            &k,
            &v,
            &g,
            &beta,
            batch,
            steps,
            heads,
            state_dim,
            &stream,
        )
        .unwrap();
    kernels
        .deltanet_gated_scan_segmented_shared_d128_f16(
            &new_output,
            &initial,
            &q,
            &k,
            &v,
            &g,
            &beta,
            batch,
            steps,
            heads,
            state_dim,
            &stream,
        )
        .unwrap();
    kernels
        .deltanet_commit_checkpoint_segmented_f32(
            &old_states,
            &checkpoints,
            &decisions,
            batch,
            steps,
            heads,
            state_dim,
            &stream,
        )
        .unwrap();
    kernels
        .deltanet_commit_recompute_segmented_shared_d128_f32(
            &new_states,
            &initial,
            &k,
            &v,
            &g,
            &beta,
            &decisions,
            batch,
            steps,
            heads,
            state_dim,
            &stream,
        )
        .unwrap();
    device.synchronize().unwrap();

    let mut old_output_bytes = vec![0u8; value_elements * 2];
    let mut new_output_bytes = vec![0u8; value_elements * 2];
    let mut old_state_bytes = vec![0u8; batch * state_elements * 4];
    let mut new_state_bytes = vec![0u8; batch * state_elements * 4];
    device.read(&old_output, 0, &mut old_output_bytes).unwrap();
    device.read(&new_output, 0, &mut new_output_bytes).unwrap();
    device.read(&old_states, 0, &mut old_state_bytes).unwrap();
    device.read(&new_states, 0, &mut new_state_bytes).unwrap();
    assert_eq!(new_output_bytes, old_output_bytes);
    assert_eq!(new_state_bytes, old_state_bytes);
}

#[test]
fn kernele_b8_nie_czytaja_ani_nie_zapisuja_nieaktywnych_wierszy() {
    let Some(device) = device() else { return };
    let kernels = Kernels::load(device.clone()).unwrap();
    let stream = device.create_stream().unwrap();
    let rows = 9usize;
    let cols = 64usize;
    let guard_elements = 17usize;
    let mut q8_weights = vec![0u8; rows * (cols / 32) * 34];
    for block in q8_weights.chunks_exact_mut(34) {
        block[..2].copy_from_slice(&f16::ONE.to_bits().to_le_bytes());
        block[2..].fill(1);
    }
    let nvfp4_weights = vec![0u8; rows * (cols / 64) * 36];
    let q8 = device
        .alloc(q8_weights.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    let nvfp4 = device
        .alloc(nvfp4_weights.len(), MemKind::Device, Pool::Weights)
        .unwrap();
    device.write(&q8_weights, &q8, 0).unwrap();
    device.write(&nvfp4_weights, &nvfp4, 0).unwrap();

    for tokens in [6usize, 8] {
        let input_values: Vec<f16> = (0..tokens * cols)
            .map(|index| f16::from_f32((index % 31) as f32 * 0.01 - 0.15))
            .collect();
        let input = device
            .alloc(input_values.len() * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        device
            .write(bytemuck::cast_slice(&input_values), &input, 0)
            .unwrap();

        let f16_canary = f16::from_bits(0x7bcd);
        let f16_output_values = vec![f16_canary; tokens * rows + guard_elements];
        let q8_output = device
            .alloc(
                f16_output_values.len() * 2,
                MemKind::Device,
                Pool::Activations,
            )
            .unwrap();
        let nvfp4_output = device
            .alloc(
                f16_output_values.len() * 2,
                MemKind::Device,
                Pool::Activations,
            )
            .unwrap();
        device
            .write(bytemuck::cast_slice(&f16_output_values), &q8_output, 0)
            .unwrap();
        device
            .write(bytemuck::cast_slice(&f16_output_values), &nvfp4_output, 0)
            .unwrap();
        kernels
            .gemm_q8_0_i8mma_at(&q8_output, &q8, 0, &input, rows, cols, tokens, &stream)
            .unwrap();
        kernels
            .gemm_nvfp4_gguf_f16(
                &nvfp4_output,
                &nvfp4,
                &input,
                rows,
                cols,
                tokens,
                1.0,
                &stream,
            )
            .unwrap();

        let f32_canary = f32::from_bits(0x7fc0_1234);
        let exact_output_values = vec![f32_canary; tokens * rows + guard_elements];
        let exact_output = device
            .alloc(
                exact_output_values.len() * 4,
                MemKind::Device,
                Pool::Activations,
            )
            .unwrap();
        device
            .write(bytemuck::cast_slice(&exact_output_values), &exact_output, 0)
            .unwrap();
        kernels
            .gemm_q8_0_f16_exact_out_f32_at(
                &exact_output,
                &q8,
                0,
                &input,
                rows,
                cols,
                tokens,
                &stream,
            )
            .unwrap();
        device.synchronize().unwrap();

        for output in [&q8_output, &nvfp4_output] {
            let mut bytes = vec![0u8; f16_output_values.len() * 2];
            device.read(output, 0, &mut bytes).unwrap();
            let values = bytemuck::cast_slice::<u8, f16>(&bytes);
            assert!(values[..tokens * rows]
                .iter()
                .all(|value| *value != f16_canary));
            assert!(values[tokens * rows..]
                .iter()
                .all(|value| *value == f16_canary));
        }
        let mut exact_bytes = vec![0u8; exact_output_values.len() * 4];
        device.read(&exact_output, 0, &mut exact_bytes).unwrap();
        let exact_values = bytemuck::cast_slice::<u8, f32>(&exact_bytes);
        assert!(exact_values[..tokens * rows]
            .iter()
            .all(|value| value.to_bits() != f32_canary.to_bits()));
        assert!(exact_values[tokens * rows..]
            .iter()
            .all(|value| value.to_bits() == f32_canary.to_bits()));
    }
}

#[test]
fn segmentowany_join_mtp_zachowuje_osobny_initial_hidden_dla_k2_i_k3() {
    let Some(device) = device() else { return };
    let kernels = Kernels::load(device.clone()).unwrap();
    let stream = device.create_stream().unwrap();
    let batch = 2usize;
    let hidden = 64usize;
    let eps = 1e-6f32;
    for t in [3usize, 4] {
        let total = batch * t;
        let embeddings: Vec<f16> = (0..total * hidden)
            .map(|index| f16::from_f32(0.25 + (index % hidden) as f32 * 0.002))
            .collect();
        let target: Vec<f16> = (0..total * hidden)
            .map(|index| {
                let lane = index / (t * hidden);
                let row = (index / hidden) % t;
                f16::from_f32(1.0 + lane as f32 * 3.0 + row as f32 * 0.5)
            })
            .collect();
        let initial: Vec<f16> = (0..batch * hidden)
            .map(|index| f16::from_f32(if index < hidden { 2.0 } else { 7.0 }))
            .collect();
        let norm = vec![f16::ONE; hidden];
        let canary = f16::from_bits(0x7bcd);
        let guard = 23usize;
        let output_values = vec![canary; total * 2 * hidden + guard];
        let alloc = |bytes| {
            device
                .alloc(bytes, MemKind::Device, Pool::Activations)
                .unwrap()
        };
        let embedding_buffer = alloc(embeddings.len() * 2);
        let target_buffer = alloc(target.len() * 2);
        let initial_buffer = alloc(initial.len() * 2);
        let norm_buffer = alloc(norm.len() * 2);
        let output = alloc(output_values.len() * 2);
        device
            .write(bytemuck::cast_slice(&embeddings), &embedding_buffer, 0)
            .unwrap();
        device
            .write(bytemuck::cast_slice(&target), &target_buffer, 0)
            .unwrap();
        device
            .write(bytemuck::cast_slice(&initial), &initial_buffer, 0)
            .unwrap();
        device
            .write(bytemuck::cast_slice(&norm), &norm_buffer, 0)
            .unwrap();
        device
            .write(bytemuck::cast_slice(&output_values), &output, 0)
            .unwrap();
        kernels
            .mtp_norm_join_shifted_segmented_f16(
                &output,
                &embedding_buffer,
                &target_buffer,
                &initial_buffer,
                &norm_buffer,
                &norm_buffer,
                batch,
                t,
                hidden,
                eps,
                &stream,
            )
            .unwrap();
        device.synchronize().unwrap();
        let mut bytes = vec![0u8; output_values.len() * 2];
        device.read(&output, 0, &mut bytes).unwrap();
        let actual = bytemuck::cast_slice::<u8, f16>(&bytes);
        for lane in 0..batch {
            for row in 0..t {
                let input_row = lane * t + row;
                let embedding = &embeddings[input_row * hidden..(input_row + 1) * hidden];
                let hidden_row = if row == 0 {
                    &initial[lane * hidden..(lane + 1) * hidden]
                } else {
                    &target[(input_row - 1) * hidden..input_row * hidden]
                };
                let embedding_inv = (embedding
                    .iter()
                    .map(|value| value.to_f32().powi(2))
                    .sum::<f32>()
                    / hidden as f32
                    + eps)
                    .sqrt()
                    .recip();
                let hidden_inv = (hidden_row
                    .iter()
                    .map(|value| value.to_f32().powi(2))
                    .sum::<f32>()
                    / hidden as f32
                    + eps)
                    .sqrt()
                    .recip();
                let output_row = &actual[input_row * 2 * hidden..(input_row + 1) * 2 * hidden];
                for index in 0..hidden {
                    assert!(
                        (output_row[index].to_f32() - embedding[index].to_f32() * embedding_inv)
                            .abs()
                            < 0.002
                    );
                    assert!(
                        (output_row[hidden + index].to_f32()
                            - hidden_row[index].to_f32() * hidden_inv)
                            .abs()
                            < 0.002
                    );
                }
            }
        }
        assert!(actual[total * 2 * hidden..]
            .iter()
            .all(|value| *value == canary));
    }
}

#[test]
fn maskowany_append_i_metadane_mtp_obsluguja_macierz_retained_1_do_t() {
    let Some(device) = device() else { return };
    let kernels = Kernels::load(device.clone()).unwrap();
    let stream = device.create_stream().unwrap();
    let batch = 2usize;
    let page_size = 2usize;
    let max_pages = 2usize;
    let head_dim = 32usize;
    let canary = f16::from_bits(0x7bcd);
    for t in [3usize, 4] {
        let total = batch * t;
        let input: Vec<f16> = (0..total * head_dim)
            .map(|index| f16::from_f32((index / head_dim + 1) as f32))
            .collect();
        let input_buffer = device
            .alloc(input.len() * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        let page_tables = device
            .alloc(batch * max_pages * 4, MemKind::Device, Pool::Activations)
            .unwrap();
        let bases = device
            .alloc(batch * 4, MemKind::Device, Pool::Activations)
            .unwrap();
        let decisions = device
            .alloc(batch * 2 * 4, MemKind::Device, Pool::Activations)
            .unwrap();
        let seq_lens = device
            .alloc((batch + 3) * 4, MemKind::Device, Pool::Activations)
            .unwrap();
        let positions = device
            .alloc((batch + 3) * 4, MemKind::Device, Pool::Activations)
            .unwrap();
        let cache_elements = batch * max_pages * page_size * head_dim;
        let k_cache = device
            .alloc(cache_elements * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        let v_cache = device
            .alloc(cache_elements * 2, MemKind::Device, Pool::Activations)
            .unwrap();
        device
            .write(bytemuck::cast_slice(&input), &input_buffer, 0)
            .unwrap();
        device
            .write(bytemuck::cast_slice(&[0i32, 1, 2, 3]), &page_tables, 0)
            .unwrap();
        device
            .write(bytemuck::cast_slice(&[0i32, 0]), &bases, 0)
            .unwrap();
        for retained0 in 1..=t {
            for retained1 in 1..=t {
                let decisions_host = [retained0 as i32, 101, retained1 as i32, 202];
                device
                    .write(bytemuck::cast_slice(&decisions_host), &decisions, 0)
                    .unwrap();
                let cache = vec![canary; cache_elements];
                device
                    .write(bytemuck::cast_slice(&cache), &k_cache, 0)
                    .unwrap();
                device
                    .write(bytemuck::cast_slice(&cache), &v_cache, 0)
                    .unwrap();
                let metadata_canary = vec![0x6bad_cafeu32; batch + 3];
                device
                    .write(bytemuck::cast_slice(&metadata_canary), &seq_lens, 0)
                    .unwrap();
                device
                    .write(bytemuck::cast_slice(&metadata_canary), &positions, 0)
                    .unwrap();
                kernels
                    .kv_append_batch_segmented_masked_f16(
                        &k_cache,
                        &v_cache,
                        &input_buffer,
                        &input_buffer,
                        &page_tables,
                        &bases,
                        &decisions,
                        batch,
                        t,
                        max_pages,
                        1,
                        page_size,
                        head_dim,
                        &stream,
                    )
                    .unwrap();
                kernels
                    .mtp_commit_catchup_metadata_segmented(
                        &seq_lens, &positions, &bases, &decisions, batch, &stream,
                    )
                    .unwrap();
                device.synchronize().unwrap();
                let mut cache_bytes = vec![0u8; cache_elements * 2];
                device.read(&k_cache, 0, &mut cache_bytes).unwrap();
                let actual = bytemuck::cast_slice::<u8, f16>(&cache_bytes);
                for lane in 0..batch {
                    let retained = [retained0, retained1][lane];
                    for row in 0..t {
                        let page = lane * max_pages + row / page_size;
                        let slot = row % page_size;
                        let offset = (page * page_size + slot) * head_dim;
                        let expected = if row < retained {
                            f16::from_f32((lane * t + row + 1) as f32)
                        } else {
                            canary
                        };
                        assert!(actual[offset..offset + head_dim]
                            .iter()
                            .all(|value| *value == expected));
                    }
                }
                for (buffer, expected) in [
                    (&seq_lens, [retained0 as i32, retained1 as i32]),
                    (&positions, [retained0 as i32 - 1, retained1 as i32 - 1]),
                ] {
                    let mut bytes = vec![0u8; (batch + 3) * 4];
                    device.read(buffer, 0, &mut bytes).unwrap();
                    let values = bytemuck::cast_slice::<u8, i32>(&bytes);
                    assert_eq!(&values[..batch], &expected);
                    assert!(values[batch..]
                        .iter()
                        .all(|value| *value as u32 == 0x6bad_cafe));
                }
            }
        }
    }
}
