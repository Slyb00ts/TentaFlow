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

use forge_formats::nvfp4::{
    deepseek_expert_to_gguf, e2m1_to_f32, f8e4m3_to_f32, DeepseekNvFp4Names,
};
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

/// Wagi nieekspertowe przechodzą z kafelkowej skali E8M0 na skalę na wiersz,
/// bo FORGE ma kernel FP8 tylko w tym drugim wariancie. Test mierzy, ile ta
/// zmiana kosztuje na wyjściu projekcji — czyli na tym, co widzi model.
///
/// Mierzona jest też odrzucona alternatywa: przekwantyzowanie na Q8_0 mieściło
/// się w tym samym bajcie na wagę, ale kosztowało 5,4e-3, bo int8 z jedną skalą
/// na 32 elementy zeruje wartości dużo mniejsze od maksimum grupy. Materializacja
/// do f16 nie kosztuje dokładności, ale urosłaby te wagi z 8,2 do 13,7 GiB przy
/// karcie mającej 16 GiB.
#[test]
fn fp8_row_scaling_barely_changes_the_projection() {
    let Some(st) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu DeepSeek V4 (FORGE_DEEPSEEK_V4_DIR)");
        return;
    };
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
        let scale_cols = scale_info.shape[1];
        assert_eq!(
            rows / scale_info.shape[0],
            tile,
            "{name}: kafel nie jest kwadratowy"
        );

        let (bytes, row_scales) =
            forge_formats::nvfp4::deepseek_fp8_to_row_scaled(weight, scales, rows, cols, tile)
                .expect(name);
        assert_eq!(bytes.len(), rows * cols, "{name}: nadal jeden bajt na wagę");
        assert_eq!(row_scales.len(), rows);

        // Aktywacja o rozkładzie zbliżonym do wyjścia normy RMS: zerowa średnia,
        // jednostkowa skala, deterministyczna.
        let x: Vec<f32> = (0..cols)
            .map(|i| (((i * 2654435761usize) % 2003) as f32 / 1001.5) - 1.0)
            .collect();

        let mut num = 0f64;
        let mut den = 0f64;
        for row in 0..rows {
            let mut exact = 0f64;
            let mut got = 0f64;
            for col in 0..cols {
                let scale = forge_formats::nvfp4::f8e8m0_to_f32(
                    scales[(row / tile) * scale_cols + col / tile],
                )
                .unwrap();
                exact += (f8e4m3_to_f32(weight[row * cols + col]) * scale * x[col]) as f64;
                got += (f8e4m3_to_f32(bytes[row * cols + col]) * row_scales[row] * x[col]) as f64;
            }
            num += (got - exact).powi(2);
            den += exact.powi(2);
        }
        let rel = (num / den.max(f64::MIN_POSITIVE)).sqrt();
        eprintln!("{name}: wzgledny blad wyjscia projekcji = {rel:.3e}");
        assert!(
            rel < 1e-5,
            "{name}: przeniesienie skali na wiersz zmienia wyjscie o {rel:.3e}"
        );
    }
}

/// Skala kafla równa NaN oznacza uszkodzony checkpoint.
#[test]
fn row_scaling_rejects_nan_tile_scale() {
    let weight = vec![0x38u8; 64];
    let mut scales = vec![127u8; 2];
    scales[1] = 0xFF;
    assert!(forge_formats::nvfp4::deepseek_fp8_to_row_scaled(&weight, &scales, 1, 64, 32).is_err());
}
