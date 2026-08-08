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
    // do granulacji alokatora. Ósemka pokrywa oba z zapasem.
    let wanted = file + (file / 8) + (256 << 20);
    let host_reserve = free / 8;
    if wanted + other_pools + host_reserve > free {
        eprintln!("pominięto {reason}: checkpoint nie mieści się w pamięci");
        return None;
    }
    Some(wanted)
}
