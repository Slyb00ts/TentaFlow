// ===== File: deepseek_v4_experts.rs — eksperci NVFP4 DeepSeeka na prawdziwych wagach =====
//
// Rezydencja ekspertów wymaga wagi o JEDNYM buforze bajtów, a DeepSeek trzyma
// pakiet i skale osobno. Test sprawdza, że przepakowanie do jednobuforowego
// układu GGUF zachowuje wartości CO DO BITU — na prawdziwym tensorze, nie na
// syntetyku, bo to na prawdziwych danych wychodzą konwencje.
//
// Sprawdza też rzecz, która przy podstawieniu złej konwencji nie wywala się, a
// psuje model po cichu: `weight_scale_2` MNOŻY wynik. Odwrotna konwencja daje
// wagi rzędu 10^6 zamiast 10^-2 i model, który generuje śmieci bez jednego
// komunikatu błędu.

use std::path::PathBuf;

use forge_formats::nvfp4::{deepseek_expert_to_gguf, e2m1_to_f32, f8e4m3_to_f32, DeepseekNvFp4Names};
use forge_formats::safetensors::ShardedSafeTensors;
use forge_types::{DType, QuantKind};

fn checkpoint() -> Option<ShardedSafeTensors> {
    let dir = std::env::var("FORGE_DEEPSEEK_V4_DIR")
        .unwrap_or_else(|_| "/mnt/d/models/nvidia_DeepSeek-V4-Flash-NVFP4".to_string());
    let dir = PathBuf::from(dir);
    if !dir.join("model.safetensors.index.json").is_file() {
        return None;
    }
    Some(ShardedSafeTensors::load_dir(&dir).expect("otwarcie shardów"))
}

fn f32_scalar(st: &ShardedSafeTensors, name: &str) -> f32 {
    let bytes = st.data(name).expect(name);
    assert_eq!(bytes.len(), 4, "{name} nie jest skalarem f32");
    f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// Referencja układu źródłowego: niski nibble to element parzysty, skala
/// blokowa co 16 elementów, skala globalna przez MNOŻENIE.
fn reference(packed: &[u8], scales: &[u8], global: f32, rows: usize, cols: usize) -> Vec<f32> {
    let mut out = vec![0f32; rows * cols];
    for row in 0..rows {
        for col in 0..cols {
            let byte = packed[row * cols / 2 + col / 2];
            let code = if col % 2 == 0 { byte & 0x0F } else { byte >> 4 };
            let scale = f8e4m3_to_f32(scales[row * cols / 16 + col / 16]);
            out[row * cols + col] = e2m1_to_f32(code) * scale * global;
        }
    }
    out
}

fn check_expert(st: &ShardedSafeTensors, weight_name: &str) {
    let names = DeepseekNvFp4Names::for_weight(weight_name).unwrap();
    let info = st.tensor(&names.packed).expect(&names.packed);
    let shape = info.shape.clone();
    let packed = st.data(&names.packed).unwrap().to_vec();
    let scales = st.data(&names.scale).unwrap().to_vec();
    let global = f32_scalar(st, &names.global_scale);

    let repacked = deepseek_expert_to_gguf(&packed, &shape, &scales, global).expect(weight_name);
    assert_eq!(repacked.rows, shape[0]);
    assert_eq!(repacked.cols, shape[1] * 2);
    assert_eq!(repacked.output_scale, global);

    // Kernele mnożą wynik GEMV przez output_scale, więc referencja też mnoży.
    let decoded = forge_formats::dequant::dequantize_to_f32(
        DType::U8,
        QuantKind::NVFP4Gguf,
        &repacked.blocks,
        repacked.rows * repacked.cols,
    )
    .unwrap();
    let want = reference(&packed, &scales, global, repacked.rows, repacked.cols);
    let got: Vec<f32> = decoded.iter().map(|v| v * repacked.output_scale).collect();
    assert_eq!(got, want, "{weight_name}: przepakowanie zmieniło wartości");

    // Waga o rozsądnej skali: przy odwróconej konwencji skali globalnej byłoby
    // to rzędu 10^6, więc ten warunek łapie właśnie ten błąd.
    let mean_abs = want.iter().map(|v| v.abs()).sum::<f32>() / want.len() as f32;
    assert!(
        (1e-4..1.0).contains(&mean_abs),
        "{weight_name}: średni moduł wagi {mean_abs} jest poza zakresem wytrenowanej sieci"
    );
}

#[test]
fn expert_repack_is_bit_exact_on_real_weights() {
    let Some(st) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu DeepSeek V4 (FORGE_DEEPSEEK_V4_DIR)");
        return;
    };
    // Bramka, wejście i wyjście SwiGLU mają różne kształty — w2 jest szersze w
    // wierszach i węższe w kolumnach, więc łapie błędy w przeliczaniu bloków.
    for name in [
        "layers.2.ffn.experts.0.w1.weight",
        "layers.2.ffn.experts.0.w3.weight",
        "layers.2.ffn.experts.0.w2.weight",
        "layers.30.ffn.experts.255.w1.weight",
    ] {
        check_expert(&st, name);
    }
}

/// Skala globalna niedodatnia albo nieskończona oznacza uszkodzony checkpoint,
/// a nie wagę do przemilczenia.
#[test]
fn rejects_degenerate_global_scale() {
    let packed = vec![0u8; 32];
    let scales = vec![1u8; 4];
    for bad in [0.0f32, -1.0, f32::NAN, f32::INFINITY] {
        assert!(
            deepseek_expert_to_gguf(&packed, &[1, 32], &scales, bad).is_err(),
            "weight_scale_2 = {bad} powinno zostać odrzucone"
        );
    }
}

/// Wagi nieekspertowe idą z FP8 na Q8_0, żeby zmieścić się na karcie: f16
/// urósłby je z 8,2 do 13,7 GiB. To przekwantyzowanie, więc test MIERZY jego
/// koszt zamiast go zakładać — i pilnuje, żeby nie urósł niepostrzeżenie.
#[test]
fn fp8_to_q8_0_error_stays_bounded_on_real_weights() {
    let Some(st) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu DeepSeek V4 (FORGE_DEEPSEEK_V4_DIR)");
        return;
    };
    // Cztery różne kształty i role: zejście i wyjście LoRA Q, wspólne KV,
    // bramka eksperta dzielonego.
    for name in [
        "layers.2.attn.wq_a.weight",
        "layers.2.attn.wq_b.weight",
        "layers.2.attn.wkv.weight",
        "layers.2.ffn.shared_experts.w1.weight",
    ] {
        let info = st.tensor(name).expect(name);
        let (rows, cols) = (info.shape[0], info.shape[1]);
        let weight = st.data(name).unwrap();
        let scale_name = format!("{}.scale", name.strip_suffix(".weight").unwrap());
        let scale_info = st.tensor(&scale_name).expect(&scale_name);
        let scales = st.data(&scale_name).unwrap();
        let tile = cols / scale_info.shape[1];
        assert_eq!(
            rows / scale_info.shape[0],
            tile,
            "{name}: kafel skali nie jest kwadratowy"
        );

        let q8 = forge_formats::nvfp4::deepseek_fp8_to_q8_0(weight, scales, rows, cols, tile)
            .expect(name);
        let decoded = forge_formats::dequant::dequantize_to_f32(
            DType::U8,
            QuantKind::Q8_0,
            &q8,
            rows * cols,
        )
        .unwrap();

        let scale_cols = scale_info.shape[1];
        let mut sum_sq_err = 0f64;
        let mut sum_sq_ref = 0f64;
        let mut max_rel = 0f32;
        for row in 0..rows {
            for col in 0..cols {
                let scale =
                    forge_formats::nvfp4::f8e8m0_to_f32(scales[(row / tile) * scale_cols + col / tile])
                        .unwrap();
                let want = f8e4m3_to_f32(weight[row * cols + col]) * scale;
                let got = decoded[row * cols + col];
                sum_sq_err += ((got - want) as f64).powi(2);
                sum_sq_ref += (want as f64).powi(2);
                if want != 0.0 {
                    max_rel = max_rel.max(((got - want) / want).abs());
                }
            }
        }
        let rel_l2 = (sum_sq_err / sum_sq_ref.max(f64::MIN_POSITIVE)).sqrt();
        eprintln!("{name}: względne L2 = {rel_l2:.3e}, maks. względny = {max_rel:.3e}");
        assert!(
            rel_l2 < 1e-2,
            "{name}: przekwantyzowanie FP8 -> Q8_0 zgubiło za dużo (L2 {rel_l2:.3e})"
        );
    }
}
