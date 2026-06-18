// =============================================================================
// Plik: gpu_pin_test.rs
// Opis: Weryfikuje wybor karty GPU dla embedded llama.cpp. Laduje model z
//       tensor_split (1.0 na wybranej karcie, 0.0 na reszcie) + main_gpu i
//       sprawdza przez nvidia-smi, ze VRAM laduje sie na wskazanej karcie.
// Przyklad: cargo run --release --example gpu_pin_test --features llama -- \
//           --model model.gguf --gpu 3
// =============================================================================

use std::path::PathBuf;
use std::process::Command;

use tentaflow_wrappers::llama::silence_llama_logs;
use tentaflow_wrappers::llama_engine::{EngineConfig, LlamaEngine};

fn gpu_used_mib() -> Vec<(u32, u64)> {
    let out = Command::new("nvidia-smi")
        .args(["--query-gpu=index,memory.used", "--format=csv,noheader,nounits"])
        .output()
        .expect("nvidia-smi");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut p = l.split(',').map(str::trim);
            let idx = p.next()?.parse().ok()?;
            let used = p.next()?.parse().ok()?;
            Some((idx, used))
        })
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    silence_llama_logs();

    let mut model = PathBuf::new();
    let mut gpus: Vec<usize> = vec![0];
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--model" => model = PathBuf::from(args.next().unwrap()),
            "--gpu" => gpus = vec![args.next().unwrap().parse()?],
            "--gpus" => {
                gpus = args
                    .next()
                    .unwrap()
                    .split(',')
                    .map(|s| s.trim().parse().unwrap())
                    .collect()
            }
            _ => {}
        }
    }

    let before = gpu_used_mib();
    println!("przed: {:?}", before);

    // tensor_split: 1.0 na wybranych kartach, 0.0 na reszcie -> wyklucza pozostale.
    let max = *gpus.iter().max().unwrap();
    let mut tensor_split = vec![0.0_f32; max + 1];
    for &g in &gpus {
        tensor_split[g] = 1.0;
    }
    let main_gpu = gpus[0];

    let config = EngineConfig {
        n_seq_max: 1,
        ctx_per_seq: 512,
        n_gpu_layers: 999,
        main_gpu: main_gpu as i32,
        tensor_split,
        ..EngineConfig::default()
    };
    println!("ladowanie {} na GPU {:?}", model.display(), gpus);
    let _engine = LlamaEngine::load(&model, config)?;
    println!("zaladowano");

    let after = gpu_used_mib();
    println!("po:    {:?}", after);

    let delta: Vec<(u32, i64)> = after
        .iter()
        .zip(before.iter())
        .map(|((i, a), (_, b))| (*i, *a as i64 - *b as i64))
        .collect();
    println!("delta MiB: {:?}", delta);

    // Wagi modelu ladują się na wybranych kartach (duzy przyrost), niewybrane
    // dostaja tylko narzut kontekstu CUDA (~kilkaset MiB na kazdej widocznej karcie).
    let selected_min = gpus
        .iter()
        .map(|&g| delta.iter().find(|(i, _)| *i as usize == g).unwrap().1)
        .min()
        .unwrap();
    let other_max = delta
        .iter()
        .filter(|(i, _)| !gpus.contains(&(*i as usize)))
        .map(|(_, d)| *d)
        .max()
        .unwrap_or(0);
    println!(
        "min przyrost na wybranych: {} MiB | max na niewybranych: {} MiB",
        selected_min, other_max
    );

    if selected_min > other_max + 100 {
        println!("OK: wagi modelu na wybranych kartach {:?}", gpus);
        Ok(())
    } else {
        Err(format!(
            "FAIL: wybrane {:?} min={} MiB, niewybrane max={} MiB",
            gpus, selected_min, other_max
        )
        .into())
    }
}
