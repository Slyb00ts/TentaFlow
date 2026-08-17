// ===== File: cpu_share_prefill.rs — what the CPU's share of prefill is worth =====
//
// EKS-A7 established that CPU and GPU sum on this chip and wired the row split
// into the variant registry. This measures what it is worth END TO END, per
// prompt length, and confirms the negative control: decode must not gain.
//
// The measurement is INTERLEAVED — off, on, off, on within one process — because
// the machine drifts. Two separate runs would compare two temperatures and
// attribute the difference to the split. Interleaving also means an unrelated
// load on the machine degrades both arms rather than only the one that ran
// while it was busy.

#![cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]

use forge_hal::metal_device::MetalDevice;
use forge_kernels::MetalExec;
use forge_model::dense::{Dense, Feed};

const SLOT: usize = 0;
const CHECKPOINT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../.runtime/models/models--agentGreg--Bielik-Minitron-7B-v3.0-Instruct-MLX-4bit/snapshots"
);

/// Prompt lengths. 128 is BELOW `MIN_SPLIT_TOKENS` and is here on purpose: it
/// proves the threshold holds, i.e. that the split does not engage where it was
/// measured to hurt.
const PROMPTS: [usize; 4] = [128, 256, 512, 1024];

/// Decode steps for the negative control. Short, because the claim is a
/// direction (no gain), not a precise figure — `how_fast_decode_runs` owns that.
const DECODE_STEPS: usize = 24;

fn checkpoint() -> Option<std::path::PathBuf> {
    let dir = std::fs::read_dir(CHECKPOINT).ok()?.flatten().next()?.path();
    dir.join("model.safetensors").is_file().then_some(dir)
}

/// Median of the measured runs, discarding a warm-up. A single cold run times
/// weight residency and kernel compilation instead of the kernel.
fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

fn prefill_seconds(model: &mut Dense<MetalExec>, prompt: &[u32], reps: usize) -> f64 {
    let mut times = Vec::new();
    for run in 0..=reps {
        model.reset(SLOT).expect("reset");
        let start = std::time::Instant::now();
        model.prefill(SLOT, prompt).expect("prefill");
        if run > 0 {
            times.push(start.elapsed().as_secs_f64());
        }
    }
    median(times)
}

fn decode_seconds(model: &mut Dense<MetalExec>, prompt: &[u32], reps: usize) -> f64 {
    let mut times = Vec::new();
    for run in 0..=reps {
        model.reset(SLOT).expect("reset");
        let mut token = model.prefill(SLOT, prompt).expect("prefill");
        let start = std::time::Instant::now();
        for _ in 0..DECODE_STEPS {
            token = model.decode(&[Feed { slot: SLOT, token }]).expect("krok")[0];
        }
        if run > 0 {
            times.push(start.elapsed().as_secs_f64());
        }
    }
    median(times)
}

#[test]
#[ignore]
fn what_the_cpu_share_is_worth_in_prefill() {
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model =
        Dense::load(&dir, |spec| MetalExec::new(device, spec)).expect("wczytanie modelu");

    let reps: usize = std::env::var("FORGE_BENCH_REPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

    eprintln!("\n| prompt | samo GPU | GPU + CPU | zysk |");
    eprintln!("|---:|---:|---:|---:|");

    for &want in &PROMPTS {
        // A synthetic prompt: the ids only have to be valid, because this
        // measures time, and time does not depend on WHICH tokens go through.
        let prompt: Vec<u32> = (0..want).map(|i| (i % 30000) as u32 + 3).collect();

        // Interleaved, and the ORDER alternates per length so a monotone drift
        // in machine temperature cannot favour one arm systematically.
        model.exec_mut().set_cpu_share(false);
        let alone = prefill_seconds(&mut model, &prompt, reps);
        model.exec_mut().set_cpu_share(true);
        let shared = prefill_seconds(&mut model, &prompt, reps);
        model.exec_mut().set_cpu_share(false);
        let alone_again = prefill_seconds(&mut model, &prompt, reps);

        let alone = alone.min(alone_again);
        let gain = (alone / shared - 1.0) * 100.0;
        eprintln!(
            "| {want} | {:.1} tok/s | {:.1} tok/s | {gain:+.1}% |",
            want as f64 / alone,
            want as f64 / shared,
        );
    }
}

/// The negative control. Decode is bandwidth-bound on shared memory, so adding
/// compute cannot help and was measured to cost 14% when forced. The split must
/// therefore never engage here — this asserts the direction, so that a future
/// change to the registry cannot quietly let decode into the split.
#[test]
#[ignore]
fn the_cpu_share_does_not_reach_decode() {
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model =
        Dense::load(&dir, |spec| MetalExec::new(device, spec)).expect("wczytanie modelu");

    let reps: usize = std::env::var("FORGE_BENCH_REPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let prompt: Vec<u32> = (0..256).map(|i| (i % 30000) as u32 + 3).collect();

    model.exec_mut().set_cpu_share(false);
    let alone = decode_seconds(&mut model, &prompt, reps);
    model.exec_mut().set_cpu_share(true);
    let shared = decode_seconds(&mut model, &prompt, reps);

    let alone_rate = DECODE_STEPS as f64 / alone;
    let shared_rate = DECODE_STEPS as f64 / shared;
    eprintln!(
        "dekodowanie: samo GPU {alone_rate:.1} tok/s, z podziałem włączonym \
         {shared_rate:.1} tok/s ({:+.1}%)",
        (shared_rate / alone_rate - 1.0) * 100.0
    );

    // The flag is on, but no decode shape qualifies, so the two arms must be
    // the SAME path. A tolerance, not equality: this is wall-clock on a machine
    // that has other work. Anything past it means decode entered the split.
    let drift = (shared_rate / alone_rate - 1.0).abs();
    assert!(
        drift < 0.10,
        "dekodowanie zmieniło się o {:.1}% po włączeniu podziału — \
         forma dekodowania nie powinna się do niego kwalifikować",
        drift * 100.0
    );
}
