// =============================================================================
// Plik: kv_page_permutation.rs
// Opis: Diagnozuje niezależność append i attention od fizycznej permutacji stron KV.
// Przykład: FORGE_GPU_TEST=1 cargo test -p forge-kernels --test kv_page_permutation -- --nocapture
// =============================================================================

use std::sync::Arc;

use forge_hal::{gpu, PoolSizes};
use forge_hal::{DevBuffer, Device, Pool};
use forge_kernels::Kernels;
use forge_types::MemKind;
use half::f16;

const PAGE_SIZE: usize = 32;
const MAX_PAGES: usize = 8;
const N_PAGES: usize = 8;
const N_Q_HEADS: usize = 4;
const N_KV_HEADS: usize = 2;
const HEAD_DIM: usize = 256;
const CONTEXTS: [usize; 8] = [31, 32, 33, 64, 128, 161, 192, 256];
type LayoutResult = (Vec<u16>, Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>);

fn device() -> Option<Arc<dyn Device>> {
    if std::env::var("FORGE_GPU_TEST").ok().as_deref() != Some("1") {
        eprintln!("pomijam test GPU; ustaw FORGE_GPU_TEST=1");
        return None;
    }
    match gpu::open(
        0,
        PoolSizes {
            weights: 64 << 20,
            kv_cache: 64 << 20,
            activations: 64 << 20,
            kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        },
    ) {
        Ok(device) => Some(device),
        Err(error) => {
            eprintln!("pomijam test GPU: {error}");
            None
        }
    }
}

fn value(seed: usize, position: usize, head: usize, element: usize) -> f16 {
    let raw = (seed + position * 29 + head * 17 + element * 7) % 251;
    f16::from_f32((raw as f32 - 125.0) / 256.0)
}

fn upload_f16(device: &dyn Device, values: &[f16], pool: Pool) -> DevBuffer {
    let buffer = device
        .alloc(values.len() * 2, MemKind::Device, pool)
        .unwrap();
    device
        .write(bytemuck::cast_slice(values), &buffer, 0)
        .unwrap();
    buffer
}

fn download_f16_bits(device: &dyn Device, buffer: &DevBuffer, elements: usize) -> Vec<u16> {
    let mut bytes = vec![0u8; elements * 2];
    device.read(buffer, 0, &mut bytes).unwrap();
    bytes
        .chunks_exact(2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .collect()
}

fn cache_offset(page: usize, head: usize, slot: usize, element: usize) -> usize {
    ((page * N_KV_HEADS + head) * PAGE_SIZE + slot) * HEAD_DIM + element
}

fn assert_output_bits_equal(identity: &[u16], permuted: &[u16], label: &str) {
    if identity == permuted {
        return;
    }
    let first = identity
        .iter()
        .zip(permuted)
        .position(|(left, right)| left != right)
        .unwrap();
    let max_diff = identity
        .iter()
        .zip(permuted)
        .map(|(&left, &right)| {
            (f16::from_bits(left).to_f32() - f16::from_bits(right).to_f32()).abs()
        })
        .filter(|value| value.is_finite())
        .fold(0.0f32, f32::max);
    panic!(
        "output różni się dla {label}: pierwszy element={first}, identity=0x{:04x}, permuted=0x{:04x}, max_diff={max_diff}",
        identity[first], permuted[first]
    );
}

fn download_bytes(device: &dyn Device, buffer: &DevBuffer) -> Vec<u8> {
    let mut bytes = vec![0u8; buffer.len()];
    device.read(buffer, 0, &mut bytes).unwrap();
    bytes
}

fn run_layout(
    device: &Arc<dyn Device>,
    kernels: &Kernels,
    context: usize,
    page_table: &[i32; MAX_PAGES],
) -> LayoutResult {
    let cache_elements = N_PAGES * N_KV_HEADS * PAGE_SIZE * HEAD_DIM;
    let poison = f16::from_bits(0x7e01);
    let mut k_cache = vec![poison; cache_elements];
    let mut v_cache = vec![poison; cache_elements];

    for position in 0..context - 1 {
        let page = page_table[position / PAGE_SIZE] as usize;
        let slot = position % PAGE_SIZE;
        for head in 0..N_KV_HEADS {
            for element in 0..HEAD_DIM {
                let offset = cache_offset(page, head, slot, element);
                k_cache[offset] = value(3, position, head, element);
                v_cache[offset] = value(11, position, head, element);
            }
        }
    }

    let q: Vec<f16> = (0..N_Q_HEADS)
        .flat_map(|head| (0..HEAD_DIM).map(move |element| value(19, context, head, element)))
        .collect();
    let current_k: Vec<f16> = (0..N_KV_HEADS)
        .flat_map(|head| (0..HEAD_DIM).map(move |element| value(3, context - 1, head, element)))
        .collect();
    let current_v: Vec<f16> = (0..N_KV_HEADS)
        .flat_map(|head| (0..HEAD_DIM).map(move |element| value(11, context - 1, head, element)))
        .collect();

    let k_cache = upload_f16(device.as_ref(), &k_cache, Pool::KvCache);
    let v_cache = upload_f16(device.as_ref(), &v_cache, Pool::KvCache);
    let q = upload_f16(device.as_ref(), &q, Pool::Activations);
    let current_k_buf = upload_f16(device.as_ref(), &current_k, Pool::Activations);
    let current_v_buf = upload_f16(device.as_ref(), &current_v, Pool::Activations);
    let out = upload_f16(
        device.as_ref(),
        &vec![poison; N_Q_HEADS * HEAD_DIM],
        Pool::Activations,
    );
    let parts = device
        .alloc(N_Q_HEADS * 8 * 260 * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let page_table_buf = device
        .alloc(MAX_PAGES * 4, MemKind::Device, Pool::Weights)
        .unwrap();
    device
        .write(bytemuck::cast_slice(page_table), &page_table_buf, 0)
        .unwrap();
    let seq_len = device.alloc(4, MemKind::Device, Pool::Weights).unwrap();
    device
        .write(&(context as i32).to_le_bytes(), &seq_len, 0)
        .unwrap();
    let stream = device.create_stream().unwrap();

    kernels
        .kv_append_f16(
            &k_cache,
            &v_cache,
            &current_k_buf,
            &current_v_buf,
            &page_table_buf,
            &seq_len,
            N_KV_HEADS,
            PAGE_SIZE,
            HEAD_DIM,
            &stream,
        )
        .unwrap();
    kernels
        .attn_decode_f16(
            &out,
            &parts,
            &q,
            &k_cache,
            &v_cache,
            &page_table_buf,
            &seq_len,
            1,
            N_Q_HEADS,
            N_KV_HEADS,
            HEAD_DIM,
            PAGE_SIZE,
            MAX_PAGES,
            1.0 / (HEAD_DIM as f32).sqrt(),
            0,
            &stream,
        )
        .unwrap();
    device.synchronize().unwrap();

    let page = page_table[(context - 1) / PAGE_SIZE] as usize;
    let slot = (context - 1) % PAGE_SIZE;
    let mut appended_k = vec![0u8; N_KV_HEADS * HEAD_DIM * 2];
    let mut appended_v = vec![0u8; N_KV_HEADS * HEAD_DIM * 2];
    for head in 0..N_KV_HEADS {
        let source = cache_offset(page, head, slot, 0) * 2;
        let target = head * HEAD_DIM * 2;
        device
            .read(
                &k_cache,
                source,
                &mut appended_k[target..target + HEAD_DIM * 2],
            )
            .unwrap();
        device
            .read(
                &v_cache,
                source,
                &mut appended_v[target..target + HEAD_DIM * 2],
            )
            .unwrap();
    }
    (
        download_f16_bits(device.as_ref(), &out, N_Q_HEADS * HEAD_DIM),
        bytemuck::cast_slice(&current_k).to_vec(),
        bytemuck::cast_slice(&current_v).to_vec(),
        appended_k,
        appended_v,
    )
}

fn run_batch_layout(
    device: &Arc<dyn Device>,
    kernels: &Kernels,
    base: usize,
    tokens: usize,
    page_table: &[i32; MAX_PAGES],
) -> LayoutResult {
    let cache_elements = N_PAGES * N_KV_HEADS * PAGE_SIZE * HEAD_DIM;
    let poison = f16::from_bits(0x7e01);
    let mut k_cache = vec![poison; cache_elements];
    let mut v_cache = vec![poison; cache_elements];
    for position in 0..base {
        let page = page_table[position / PAGE_SIZE] as usize;
        let slot = position % PAGE_SIZE;
        for head in 0..N_KV_HEADS {
            for element in 0..HEAD_DIM {
                let offset = cache_offset(page, head, slot, element);
                k_cache[offset] = value(3, position, head, element);
                v_cache[offset] = value(11, position, head, element);
            }
        }
    }

    let q: Vec<f16> = (0..tokens)
        .flat_map(|token| {
            (0..N_Q_HEADS).flat_map(move |head| {
                (0..HEAD_DIM).map(move |element| value(19, base + token, head, element))
            })
        })
        .collect();
    let batch_k: Vec<f16> = (0..tokens)
        .flat_map(|token| {
            (0..N_KV_HEADS).flat_map(move |head| {
                (0..HEAD_DIM).map(move |element| value(3, base + token, head, element))
            })
        })
        .collect();
    let batch_v: Vec<f16> = (0..tokens)
        .flat_map(|token| {
            (0..N_KV_HEADS).flat_map(move |head| {
                (0..HEAD_DIM).map(move |element| value(11, base + token, head, element))
            })
        })
        .collect();
    let visible_lens: Vec<i32> = (base + 1..=base + tokens)
        .map(|length| length as i32)
        .collect();

    let k_cache = upload_f16(device.as_ref(), &k_cache, Pool::KvCache);
    let v_cache = upload_f16(device.as_ref(), &v_cache, Pool::KvCache);
    let q = upload_f16(device.as_ref(), &q, Pool::Activations);
    let batch_k_buf = upload_f16(device.as_ref(), &batch_k, Pool::Activations);
    let batch_v_buf = upload_f16(device.as_ref(), &batch_v, Pool::Activations);
    let out = upload_f16(
        device.as_ref(),
        &vec![poison; tokens * N_Q_HEADS * HEAD_DIM],
        Pool::Activations,
    );
    let page_table_buf = device
        .alloc(MAX_PAGES * 4, MemKind::Device, Pool::Weights)
        .unwrap();
    device
        .write(bytemuck::cast_slice(page_table), &page_table_buf, 0)
        .unwrap();
    let base_pos = device.alloc(4, MemKind::Device, Pool::Weights).unwrap();
    device
        .write(&(base as i32).to_le_bytes(), &base_pos, 0)
        .unwrap();
    let visible_lens_buf = device
        .alloc(tokens * 4, MemKind::Device, Pool::Weights)
        .unwrap();
    device
        .write(bytemuck::cast_slice(&visible_lens), &visible_lens_buf, 0)
        .unwrap();
    let stream = device.create_stream().unwrap();

    kernels
        .kv_append_batch_device_pos_f16(
            &k_cache,
            &v_cache,
            &batch_k_buf,
            &batch_v_buf,
            &page_table_buf,
            &base_pos,
            tokens,
            N_KV_HEADS,
            PAGE_SIZE,
            HEAD_DIM,
            &stream,
        )
        .unwrap();
    kernels
        .attn_decode_batch_exact_f16_hd256(
            &out,
            &q,
            &k_cache,
            &v_cache,
            &page_table_buf,
            &visible_lens_buf,
            tokens,
            N_Q_HEADS,
            N_KV_HEADS,
            PAGE_SIZE,
            MAX_PAGES,
            1.0 / (HEAD_DIM as f32).sqrt(),
            &stream,
        )
        .unwrap();
    device.synchronize().unwrap();

    let k_cache_bytes = download_bytes(device.as_ref(), &k_cache);
    let v_cache_bytes = download_bytes(device.as_ref(), &v_cache);
    let mut appended_k = Vec::with_capacity(tokens * N_KV_HEADS * HEAD_DIM * 2);
    let mut appended_v = Vec::with_capacity(tokens * N_KV_HEADS * HEAD_DIM * 2);
    for token in 0..tokens {
        let position = base + token;
        let page = page_table[position / PAGE_SIZE] as usize;
        let slot = position % PAGE_SIZE;
        for head in 0..N_KV_HEADS {
            let offset = cache_offset(page, head, slot, 0) * 2;
            appended_k.extend_from_slice(&k_cache_bytes[offset..offset + HEAD_DIM * 2]);
            appended_v.extend_from_slice(&v_cache_bytes[offset..offset + HEAD_DIM * 2]);
        }
    }

    (
        download_f16_bits(device.as_ref(), &out, tokens * N_Q_HEADS * HEAD_DIM),
        bytemuck::cast_slice(&batch_k).to_vec(),
        bytemuck::cast_slice(&batch_v).to_vec(),
        appended_k,
        appended_v,
    )
}

#[test]
fn append_i_attention_hd256_sa_niezalezne_od_permutacji_stron() {
    let Some(device) = device() else { return };
    let kernels = Kernels::load(device.clone()).unwrap();
    let identity = [0, 1, 2, 3, 4, 5, 6, 7];
    let permuted = [7, 0, 6, 1, 5, 2, 4, 3];

    for context in CONTEXTS {
        let identity_result = run_layout(&device, &kernels, context, &identity);
        let permuted_result = run_layout(&device, &kernels, context, &permuted);

        assert_output_bits_equal(
            &identity_result.0,
            &permuted_result.0,
            &format!("decode context={context}"),
        );
        assert_eq!(
            identity_result.1, identity_result.3,
            "append K identity context={context}"
        );
        assert_eq!(
            identity_result.2, identity_result.4,
            "append V identity context={context}"
        );
        assert_eq!(
            permuted_result.1, permuted_result.3,
            "append K permuted context={context}"
        );
        assert_eq!(
            permuted_result.2, permuted_result.4,
            "append V permuted context={context}"
        );
    }
}

#[test]
fn batch_append_i_exact_attention_hd256_sa_niezalezne_od_permutacji_stron() {
    let Some(device) = device() else { return };
    let kernels = Kernels::load(device.clone()).unwrap();
    let identity = [0, 1, 2, 3, 4, 5, 6, 7];
    let permuted = [7, 0, 6, 1, 5, 2, 4, 3];

    for base in [0usize, 31, 32] {
        for tokens in [31usize, 32, 33, 64, 128] {
            let identity_result = run_batch_layout(&device, &kernels, base, tokens, &identity);
            let permuted_result = run_batch_layout(&device, &kernels, base, tokens, &permuted);
            let label = format!("batch base={base} tokens={tokens}");

            assert_output_bits_equal(&identity_result.0, &permuted_result.0, &label);
            assert_eq!(
                identity_result.1, identity_result.3,
                "append K identity {label}"
            );
            assert_eq!(
                identity_result.2, identity_result.4,
                "append V identity {label}"
            );
            assert_eq!(
                permuted_result.1, permuted_result.3,
                "append K permuted {label}"
            );
            assert_eq!(
                permuted_result.2, permuted_result.4,
                "append V permuted {label}"
            );
        }
    }
}
