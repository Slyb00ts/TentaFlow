// Kalibruje obie karty i pokazuje, jak planer podzieli prace dla dekodowania
// (ograniczonego pasmem) i dla prefillu (ograniczonego liczeniem).
//
// Format wag podaje sie argumentem, bo od niego ZALEZY WYNIK: `nvfp4` daje na
// tej maszynie stosunek 1 : 8,8, a `q4k` na tych samych kartach 0,95 : 1.
// Uruchom oba, zeby zobaczyc, ze jeden staly probe nie opisuje obu przypadkow.
//
//   cargo run --release -p forge-cli --features hip --example multi_gpu_probe -- q4k
use forge_engine::multi_gpu::{MIN_USEFUL_ROWS, calibrate, plan_split, WorkKind};
use forge_hal::PoolSizes;
use forge_kernels::Kernels;
use forge_types::QuantKind;

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_else(|| "nvfp4".into());
    let quant = match arg.as_str() {
        "nvfp4" => QuantKind::NVFP4Gguf,
        "q8" => QuantKind::Q8_0,
        "q4k" => QuantKind::Q4K,
        other => panic!("nieznany format {other}: uzyj nvfp4 | q8 | q4k"),
    };
    println!("format wag: {arg}");
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
    let caps = calibrate(&devices, &kernels, quant).expect("kalibracja");
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
