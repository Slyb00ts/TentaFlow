// ===== File: tests/common/mod.rs — wspólne rozmiary pul testów GPU =====

use std::path::Path;

use half::f16;

/// Pula wag wielkości SAMEGO checkpointu, a nie całej wolnej pamięci.
///
/// Na karcie z własnym VRAM-em zabranie wszystkiego, co wolne, nic nie kosztuje:
/// host ma swoją pamięć osobno. Na GB10 pamięć GPU JEST pamięcią systemu, więc
/// ta sama arytmetyka rezerwuje niemal cały RAM maszyny — jądro wchodzi w
/// nieskończony odzysk i cały host staje, bez OOM-killera i bez logu. Pula
/// liczona z rozmiaru pliku zostawia resztę systemowi na obu rodzajach maszyn.
///
/// `None` znaczy „nie mieści się" — wołający ma wtedy pominąć test, nie ciąć
/// puli poniżej wag.
pub fn weights_pool(path: &Path, free: usize, other_pools: usize, reason: &str) -> Option<usize> {
    let file = match std::fs::metadata(path) {
        Ok(meta) => meta.len() as usize,
        Err(error) => {
            eprintln!("pominięto {reason}: nie można odczytać rozmiaru checkpointu: {error}");
            return None;
        }
    };
    // Rezydentne wagi bywają szersze od pliku: przepakowania i wyrównanie slabów
    // do granulacji alokatora. Ósemka pokrywa oba z zapasem, a płaski gigabajt
    // to, co model bierze z TEJ SAMEJ puli, a czego w pliku nie ma — u hybrydy
    // sloty stanu DeltaNet, po kilkadziesiąt megabajtów każdy. Bez tego zapasu
    // cache checkpointów po cichu nie dostaje ani jednego slotu.
    let wanted = file + (file / 8) + (1 << 30);
    let budget = free.max(reclaimable());
    let host_reserve = budget / 8;
    if wanted + other_pools + host_reserve > budget {
        eprintln!("pominięto {reason}: checkpoint nie mieści się w pamięci");
        return None;
    }
    Some(wanted)
}

/// Pamięć, którą jądro odda pod alokację, razem z cache'em stron.
///
/// Sterownik raportuje pamięć WOLNĄ, a na maszynie z pamięcią wspólną plik
/// checkpointu sam siedzi w cache'u stron po pierwszym załadowaniu — więc im
/// częściej test się uruchamiał, tym mniej „wolnej" pamięci widzi. Rezerwacja
/// nadal ma rozmiar checkpointu, więc ta liczba tylko decyduje, czy się mieści.
fn reclaimable() -> usize {
    let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") else {
        return 0;
    };
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|kb| kb.parse::<usize>().ok())
        .map_or(0, |kb| kb * 1024)
}

/// Czy batch tego checkpointu liczy to samo co ścieżka seryjna.
///
/// Zależy to od FORMATU wag, nie od poprawności kodu: dekodowanie idzie rodziną
/// GEMV, batch kaflem GEMM, a zgadzają się tylko tam, gdzie batch ma dokładny
/// kernel małego batcha (F16, Q8_0, NVFP4). K-kwanty kwantyzują aktywacje do
/// q8_1 i rozjeżdżają się o rząd wielkości ponad tolerancję. GGUF bez takiego
/// formatu nie jest błędem — jest innym checkpointem, więc test go pomija
/// zamiast padać, i mówi dlaczego.
#[allow(dead_code)]
pub fn exact_batch(model: &forge_engine::model::Model, what: &str) -> bool {
    if model.hybrid_batch_capable() {
        return true;
    }
    eprintln!(
        "pominięto {what}: checkpoint nie ma dokładnego kernela małego batcha \
         (arch={}, mtp_embedding={:?}) — kontrakt B2 wymaga wag F16, Q8_0 albo NVFP4",
        model.weights.descriptor.arch,
        model.mtp_embedding_mode(),
    );
    false
}

#[allow(dead_code)]
pub fn assert_mtp_snapshot_eq(
    actual: &[(String, usize, Vec<u8>)],
    expected: &[(String, usize, Vec<u8>)],
    context: &str,
) {
    assert_eq!(actual.len(), expected.len(), "liczba buforów {context}");
    for (
        (actual_name, actual_element_bytes, actual_bytes),
        (expected_name, expected_element_bytes, expected_bytes),
    ) in actual.iter().zip(expected)
    {
        assert_eq!(actual_name, expected_name, "nazwa bufora {context}");
        assert_eq!(
            actual_element_bytes, expected_element_bytes,
            "rozmiar elementu {actual_name} {context}"
        );
        assert_eq!(
            actual_bytes.len(),
            expected_bytes.len(),
            "długość {actual_name} {context}"
        );
        if let Some(index) = actual_bytes
            .iter()
            .zip(expected_bytes)
            .position(|(actual_byte, expected_byte)| actual_byte != expected_byte)
        {
            panic!(
                "pierwsza różnica {actual_name} {context}: bajt {index}, actual={}, expected={}",
                actual_bytes[index], expected_bytes[index]
            );
        }
    }
}
#[allow(dead_code)]
pub fn assert_mtp_snapshot_close(
    actual: &[(String, usize, Vec<u8>)],
    expected: &[(String, usize, Vec<u8>)],
    context: &str,
) {
    assert_eq!(actual.len(), expected.len(), "liczba buforów {context}");
    for (
        (actual_name, actual_element_bytes, actual_bytes),
        (expected_name, expected_element_bytes, expected_bytes),
    ) in actual.iter().zip(expected)
    {
        assert_eq!(actual_name, expected_name, "nazwa bufora {context}");
        assert_eq!(
            actual_element_bytes, expected_element_bytes,
            "rozmiar elementu {actual_name} {context}"
        );
        assert_eq!(
            actual_bytes.len(),
            expected_bytes.len(),
            "długość {actual_name} {context}"
        );
        if actual_name == "mtp.page_table" {
            assert_eq!(
                normalized_page_table(actual_bytes),
                normalized_page_table(expected_bytes),
                "logiczne mapowanie {actual_name} {context}"
            );
        } else if matches!(actual_name.as_str(), "mtp.hidden" | "mtp.k" | "mtp.v") {
            let (max_abs, rmse, _) =
                numeric_diff(actual_bytes, expected_bytes, *actual_element_bytes);
            assert!(
                max_abs <= 0.125 && rmse <= 0.01,
                "różnica numeryczna {actual_name} {context}: max_abs={max_abs}, rmse={rmse}"
            );
        } else {
            assert_eq!(
                actual_bytes, expected_bytes,
                "zawartość {actual_name} {context}"
            );
        }
    }
}
#[allow(dead_code)]
pub fn normalized_page_table(bytes: &[u8]) -> Vec<usize> {
    let mut physical = Vec::new();
    bytes
        .chunks_exact(4)
        .map(|value| i32::from_le_bytes(value.try_into().unwrap()))
        .map(|page| {
            assert!(
                page >= 0,
                "fizyczny identyfikator strony nie może być ujemny"
            );
            if let Some(index) = physical.iter().position(|&seen| seen == page) {
                index
            } else {
                physical.push(page);
                physical.len() - 1
            }
        })
        .collect()
}
#[allow(dead_code)]
pub fn numeric_diff(actual: &[u8], expected: &[u8], element_bytes: usize) -> (f32, f32, f32) {
    assert_eq!(actual.len(), expected.len());
    let mut max_abs = 0.0f32;
    let mut max_rel = 0.0f32;
    let mut squared_error = 0.0f64;
    let mut elements = 0usize;
    for (actual, expected) in actual
        .chunks_exact(element_bytes)
        .zip(expected.chunks_exact(element_bytes))
    {
        let actual = match element_bytes {
            2 => f16::from_le_bytes(actual.try_into().unwrap()).to_f32(),
            4 => f32::from_le_bytes(actual.try_into().unwrap()),
            _ => unreachable!(),
        };
        let expected = match element_bytes {
            2 => f16::from_le_bytes(expected.try_into().unwrap()).to_f32(),
            4 => f32::from_le_bytes(expected.try_into().unwrap()),
            _ => unreachable!(),
        };
        assert!(actual.is_finite() && expected.is_finite());
        let absolute = (actual - expected).abs();
        let relative = absolute / expected.abs().max(1e-3);
        max_abs = max_abs.max(absolute);
        max_rel = max_rel.max(relative);
        squared_error += f64::from(absolute) * f64::from(absolute);
        elements += 1;
    }
    let rmse = (squared_error / elements.max(1) as f64).sqrt() as f32;
    (max_abs, rmse, max_rel)
}
#[allow(dead_code)]
pub fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}
