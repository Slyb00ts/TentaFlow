// Kalibruje obie karty tym samym testem i pokazuje, jak planer podzieli pracę
// dla dekodowania (ograniczonego pamiecia) i dla prefillu (ograniczonego
// liczeniem). To jest sprawdzian, ze podzial bierze sie z POMIARU.
use forge_engine::multi_gpu::{MIN_USEFUL_ROWS, calibrate, plan_split, WorkKind};
use forge_hal::PoolSizes;
use forge_kernels::Kernels;

fn main() {
    let pools = PoolSizes {
        weights: 1 << 30,
        kv_cache: 16 << 20,
        activations: 64 << 20,
        kv_page_size: 256 << 10,
    };
    let mut devices = Vec::new();
    for ordinal in 0..2 {
        devices.push(forge_hal::gpu::open(ordinal, pools).expect("otwarcie karty"));
    }
    let kernels: Vec<Kernels> = devices.iter().map(|d| Kernels::load(d.clone()).expect("artefakty")).collect();
    let caps = calibrate(&devices, &kernels).expect("kalibracja");
    for (index, cap) in caps.iter().enumerate() {
        println!(
            "dev{index} {:<22} pasmo {:>6.0} GB/s  liczenie {:>6.1} TOPS  wolne {:>5} MiB",
            devices[index].caps().name,
            cap.stream_bytes_per_s / 1e9,
            cap.matmul_ops_per_s / 1e12,
            cap.free_bytes >> 20
        );
    }
    for (kind, label) in [
        (WorkKind::MemoryBound, "dekodowanie"),
        (WorkKind::ComputeBound, "prefill"),
    ] {
        let plan = plan_split(&caps, 17408, kind, 0, MIN_USEFUL_ROWS).expect("podzial");
        let shares: Vec<String> = plan
            .rows
            .iter()
            .map(|r| format!("{:.1}%", 100.0 * *r as f64 / 17408.0))
            .collect();
        println!("{label:12}: {:?} wierszy = {}", plan.rows, shares.join(" / "));
    }
}
