// ===== File: tests/common/mod.rs — wspólne rozmiary pul testów GPU =====

use std::path::Path;

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
