// ===== File: bench.rs — what the ops-as-data path actually sustains =====
//
// The correctness tests print a throughput because it is free to print, but
// what they measure is a COLD run: the first prompt through a freshly loaded
// model pays for every one-time cost there is — module loads, scratch that is
// allocated on first demand, and the second weight form that `cuda_exec::fp8`
// builds when a projection is first multiplied wide. Reading a steady-state
// number off that is how a path gets called fast or slow for the wrong reason.
//
// So: one warm-up that is thrown away, then repeats, then the middle of the
// distribution.
//
// The two numbers are the two `llama-bench` reports, measured the way it
// measures them: `pp<N>` is a prompt of N tokens divided by the time to ingest
// it, `tg<N>` is N single-token steps AFTER a prompt divided by their time.
// Deliberately the same definitions — comparing this path against another
// engine is the entire reason the numbers exist, and two different meanings of
// "tokens per second" would make the comparison say nothing.

use std::path::PathBuf;
use std::sync::Arc;

use forge_hal::{cuda::CudaDevice, PoolSizes};
use forge_kernels::CudaExec;
use forge_model::dense::{Dense, Feed};

/// Prompt content does not change the arithmetic — every token walks the same
/// projections — so the ids are synthetic and only the LENGTH is an input.
fn prompt_of(len: usize, vocab: u32) -> Vec<u32> {
    (0..len).map(|i| (i as u32 * 7919 % vocab).max(1)).collect()
}

fn median(mut samples: Vec<f64>) -> (f64, f64, f64) {
    samples.sort_by(f64::total_cmp);
    let at = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    (at(0.1), at(0.5), at(0.9))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!("użycie: bench <gguf|katalog> [tokeny promptu] [tokeny generacji] [powtórzenia]");
        std::process::exit(2);
    };
    let prompt_tokens: usize = args.next().map_or(512, |v| v.parse().expect("tokeny promptu"));
    let gen_tokens: usize = args.next().map_or(128, |v| v.parse().expect("tokeny generacji"));
    let reps: usize = args.next().map_or(5, |v| v.parse().expect("liczba powtórzeń"));

    let device = CudaDevice::new(0, pools()).expect("urządzenie CUDA");
    let t = std::time::Instant::now();
    let mut model = Dense::load(&path, |spec| CudaExec::new(device.clone() as Arc<_>, spec))
        .expect("wczytanie modelu");
    println!("wczytane w {:.1} s", t.elapsed().as_secs_f64());

    let prompt = prompt_of(prompt_tokens, model.shape().vocab);
    // Thrown away on purpose: this is the run that pays for the fp8 packs.
    model.prefill(0, &prompt).expect("rozgrzewka");
    model.reset(0).expect("reset");

    let mut prefill = Vec::with_capacity(reps);
    let mut generate = Vec::with_capacity(reps);
    for _ in 0..reps {
        let t = std::time::Instant::now();
        let mut token = model.prefill(0, &prompt).expect("prefill");
        prefill.push(prompt.len() as f64 / t.elapsed().as_secs_f64());

        // Generation is timed SEPARATELY and after the prompt, because a step
        // reads the context the prompt left behind: measuring it from an empty
        // slot would measure a shorter context than any real answer has.
        let t = std::time::Instant::now();
        for _ in 0..gen_tokens {
            token = model.decode(&[Feed { slot: 0, token }]).expect("krok")[0];
        }
        generate.push(gen_tokens as f64 / t.elapsed().as_secs_f64());
        model.reset(0).expect("reset");
    }
    for (what, samples) in [
        (format!("pp{prompt_tokens}"), prefill),
        (format!("tg{gen_tokens}"), generate),
    ] {
        let (p10, med, p90) = median(samples);
        println!("{what} × {reps}: p10 {p10:.1} | mediana {med:.1} | p90 {p90:.1} tok/s");
    }
}

/// Pools claimed for the measurement.
///
/// Explicit rather than a share of free VRAM: the weights pool has to hold the
/// source blocks AND the e4m3 packs built on top of them, and a number that
/// moves with whatever else the machine is doing would make two runs of this
/// bench incomparable.
fn pools() -> PoolSizes {
    PoolSizes {
        weights: 24 << 30,
        kv_cache: 2 << 30,
        kv_page_size: PoolSizes::DEFAULT_KV_PAGE,
        activations: 1 << 30,
    }
}
