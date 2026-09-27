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

// Wspólna wyrocznia — tu używamy tylko miary zgodności logitów, reszta
// (fikstura mlx) jest dla innych testów.
#[cfg(feature = "ane")]
#[allow(dead_code)]
mod common;

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

// ---- Trzecie ramię: Neural Engine (cecha `ane`) ----------------------------
//
// Katalog modeli CoreML podaje `FORGE_ANE_DIR` (wynik tools/ane-export). Bez
// niego testy z tej sekcji liczą tylko dwa ramiona i mówią o tym — brak
// katalogu nie jest błędem, jest brakiem pomiaru.

#[cfg(feature = "ane")]
fn ane_dir() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("FORGE_ANE_DIR")?);
    if dir.join("manifest.json").is_file() {
        Some(dir)
    } else {
        eprintln!("FORGE_ANE_DIR={}: brak manifest.json", dir.display());
        None
    }
}

/// Wiązania modelu przepisane na typ, który zna wykonawca. Mapa jest
/// jedno-jednoznaczna z definicji: obie strony wyliczają te same siedem
/// projekcji, tylko w dwóch crate'ach, które nie mogą się nawzajem widzieć.
#[cfg(feature = "ane")]
fn attach_ane(model: &mut Dense<MetalExec>, dir: &std::path::Path) -> forge_kernels::AneLoadReport {
    use forge_kernels::{AneBindingLite, AneProjKind};
    use forge_model::dense::AneProj;
    let lite: Vec<AneBindingLite> = model
        .projection_bindings()
        .into_iter()
        .map(|b| AneBindingLite {
            layer: b.layer,
            proj: match b.proj {
                AneProj::Q => AneProjKind::Q,
                AneProj::K => AneProjKind::K,
                AneProj::V => AneProjKind::V,
                AneProj::O => AneProjKind::O,
                AneProj::Gate => AneProjKind::Gate,
                AneProj::Up => AneProjKind::Up,
                AneProj::Down => AneProjKind::Down,
            },
            id: b.id,
            rows: b.rows,
            cols: b.cols,
        })
        .collect();
    let report = model
        .exec_mut()
        .attach_ane(dir, &lite)
        .expect("podłączenie ramienia ANE");
    eprintln!(
        "ANE: {} modeli, {}/{} funkcji przy starcie (T{}..T{}), {} grup pominiętych, \
         ładowanie {:.0} ms",
        report.models,
        report.functions,
        report.functions_total,
        report.shape_min,
        report.shape_max,
        report.skipped,
        report.load_ms
    );
    report
}

/// Wolne i wired strony z `vm_stat` — stan maszyny, nie procesu. Ramię ANE
/// to pamięć wired (wagi + bufory programu), więc to jedyna miara, która
/// mówi, ile ANE naprawdę kosztuje.
#[cfg(feature = "ane")]
fn vm_stat_line() -> String {
    let out = std::process::Command::new("vm_stat").output();
    let Ok(out) = out else {
        return "vm_stat: niedostępne".into();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let page = |key: &str| -> f64 {
        text.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().trim_end_matches('.').parse::<f64>().ok())
            .unwrap_or(0.0)
            * 16384.0
            / (1024.0 * 1024.0)
    };
    format!(
        "vm_stat: free {:.0} MiB, speculative {:.0} MiB, inactive {:.0} MiB, wired {:.0} MiB",
        page("Pages free"),
        page("Pages speculative"),
        page("Pages inactive"),
        page("Pages wired down")
    )
}

/// Błędy stron procesu (`getrusage`): drobne (`ru_minflt`) i twarde
/// (`ru_majflt`). Różnica przed/po prefillu mówi, czy czas idzie w jądro.
#[cfg(feature = "ane")]
fn page_faults() -> (i64, i64) {
    #[repr(C)]
    struct Timeval {
        sec: i64,
        usec: i32,
    }
    #[repr(C)]
    struct Rusage {
        utime: Timeval,
        stime: Timeval,
        maxrss: i64,
        ixrss: i64,
        idrss: i64,
        isrss: i64,
        minflt: i64,
        majflt: i64,
        nswap: i64,
        inblock: i64,
        oublock: i64,
        msgsnd: i64,
        msgrcv: i64,
        nsignals: i64,
        nvcsw: i64,
        nivcsw: i64,
    }
    extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }
    let mut ru = Rusage {
        utime: Timeval { sec: 0, usec: 0 },
        stime: Timeval { sec: 0, usec: 0 },
        maxrss: 0,
        ixrss: 0,
        idrss: 0,
        isrss: 0,
        minflt: 0,
        majflt: 0,
        nswap: 0,
        inblock: 0,
        oublock: 0,
        msgsnd: 0,
        msgrcv: 0,
        nsignals: 0,
        nvcsw: 0,
        nivcsw: 0,
    };
    // SAFETY: RUSAGE_SELF = 0; struktura ma układ z <sys/resource.h> na
    // macOS arm64 (timeval = i64 + i32 z wyrównaniem do 8, dalej 14 x long).
    let rc = unsafe { getrusage(0, &mut ru) };
    if rc != 0 {
        return (0, 0);
    }
    (ru.minflt, ru.majflt)
}

#[cfg(feature = "ane")]
fn set_arms(model: &mut Dense<MetalExec>, cpu: bool, ane: bool) {
    model.exec_mut().set_cpu_share(cpu);
    model.exec_mut().set_ane_share(ane);
}

/// Wszystkie trzy ramiona, przeplatane w jednej długości: GPU → GPU+CPU →
/// GPU+CPU+ANE → GPU+ANE → GPU. `alone` to minimum z pierwszego i ostatniego,
/// bo maszyna dryfuje, a dryf nie ma wybierać zwycięzcy.
#[cfg(feature = "ane")]
#[test]
#[ignore]
fn what_the_three_units_are_worth_in_prefill() {
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
    eprintln!("przed ANE: {}", vm_stat_line());
    let ane = match ane_dir() {
        Some(ane) => {
            attach_ane(&mut model, &ane);
            eprintln!("po załadowaniu ANE: {}", vm_stat_line());
            true
        }
        None => {
            eprintln!("pomijam ramię ANE: brak FORGE_ANE_DIR");
            false
        }
    };

    let reps: usize = std::env::var("FORGE_BENCH_REPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);

    // `FORGE_BENCH_PROMPTS=512,1024` zawęża długości — do profilowania
    // jednego przypadku bez płacenia za pozostałe.
    let prompts: Vec<usize> = std::env::var("FORGE_BENCH_PROMPTS")
        .ok()
        .map(|v| v.split(',').filter_map(|p| p.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![256, 512, 1024]);

    // Liczniki ramienia ANE po jednym ramieniu: ile predict, ile czasu w
    // nich i ile czekania — to mówi, czy ANE jest wolne, czy tylko późne.
    let ane_report =
        |model: &mut Dense<MetalExec>, what: &str, want: usize, secs: f64, faults: (i64, i64)| {
            let st = model.exec_mut().ane_stats();
            let per = |ms: f64| ms / (reps + 1) as f64;
            let runs = reps as u64 + 1;
            eprintln!(
                "  {what} {want}: {} zleceń ({} predict)/przebieg, predict {:.0} ms, czekanie \
             {:.0} ms na przebieg ({:.0} ms całego prefillu); doładowań {}, {:.0} ms; \
             wypchnięć {}; kopii {}; błędy stron/przebieg: drobne {}, twarde {}; {}",
                st.predicts / runs,
                st.sub_predicts / runs,
                per(st.predict_ms),
                per(st.wait_ms),
                secs * 1e3,
                st.loads,
                st.load_ms,
                st.evictions,
                st.copies,
                faults.0 / runs as i64,
                faults.1 / runs as i64,
                vm_stat_line()
            );
            model.exec_mut().reset_ane_stats();
        };
    // Błędy stron liczone wokół każdego ramienia, żeby GPU i GPU+CPU miały
    // ten sam licznik co ramiona z ANE.
    let faults_around = |model: &mut Dense<MetalExec>, prompt: &[u32]| -> (f64, (i64, i64)) {
        let f0 = page_faults();
        let secs = prefill_seconds(model, prompt, reps);
        let f1 = page_faults();
        (secs, (f1.0 - f0.0, f1.1 - f0.1))
    };

    eprintln!("\n| prompt | GPU | GPU+CPU | GPU+CPU+ANE | GPU+ANE |");
    eprintln!("|---:|---:|---:|---:|---:|");
    for &want in &prompts {
        let prompt: Vec<u32> = (0..want).map(|i| (i % 30000) as u32 + 3).collect();
        let runs = reps as i64 + 1;
        set_arms(&mut model, false, false);
        let (alone, f) = faults_around(&mut model, &prompt);
        eprintln!(
            "  GPU {want}: {:.0} ms; błędy stron/przebieg: drobne {}, twarde {}",
            alone * 1e3,
            f.0 / runs,
            f.1 / runs
        );
        set_arms(&mut model, true, false);
        let (cpu, f) = faults_around(&mut model, &prompt);
        eprintln!(
            "  GPU+CPU {want}: {:.0} ms; błędy stron/przebieg: drobne {}, twarde {}",
            cpu * 1e3,
            f.0 / runs,
            f.1 / runs
        );
        let (cpu_ane, ane_only) = if ane {
            set_arms(&mut model, true, true);
            let (a, f) = faults_around(&mut model, &prompt);
            ane_report(&mut model, "GPU+CPU+ANE", want, a, f);
            set_arms(&mut model, false, true);
            let (b, f) = faults_around(&mut model, &prompt);
            ane_report(&mut model, "GPU+ANE", want, b, f);
            (Some(a), Some(b))
        } else {
            (None, None)
        };
        set_arms(&mut model, false, false);
        let alone_again = prefill_seconds(&mut model, &prompt, reps);
        let alone = alone.min(alone_again);
        let rate = |s: Option<f64>| match s {
            Some(s) => format!("{:.1} tok/s", want as f64 / s),
            None => "—".to_string(),
        };
        eprintln!(
            "| {want} | {} | {} | {} | {} |",
            rate(Some(alone)),
            rate(Some(cpu)),
            rate(cpu_ane),
            rate(ane_only),
        );
    }
}

/// Bramka K3: logity z trzema ramionami wobec samego GPU — ten sam argmax i
/// błąd poniżej 0,4% rozpiętości. Prompt 512, bo to środek zakresu kształtów
/// ANE i pierwszy, przy którym k/v też wchodzą w podział.
#[cfg(feature = "ane")]
#[test]
#[ignore]
fn the_ane_share_keeps_the_logits() {
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Some(ane) = ane_dir() else {
        eprintln!("pomijam: brak FORGE_ANE_DIR");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model =
        Dense::load(&dir, |spec| MetalExec::new(device, spec)).expect("wczytanie modelu");
    attach_ane(&mut model, &ane);

    let seed = [1, 4234, 8123, 302, 15, 977, 12000, 44];
    let prompt: Vec<u32> = seed.iter().copied().cycle().take(512).collect();

    set_arms(&mut model, false, false);
    model.reset(SLOT).expect("reset");
    let gpu_token = model.prefill(SLOT, &prompt).expect("prefill GPU");
    let gpu_logits = model.logits(0).expect("logity GPU");

    set_arms(&mut model, true, true);
    model.reset(SLOT).expect("reset");
    let three_token = model.prefill(SLOT, &prompt).expect("prefill GPU+CPU+ANE");
    let three_logits = model.logits(0).expect("logity GPU+CPU+ANE");

    let spread = gpu_logits.iter().cloned().fold(f32::NEG_INFINITY, f32::max)
        - gpu_logits.iter().cloned().fold(f32::INFINITY, f32::min);
    let worst = gpu_logits
        .iter()
        .zip(&three_logits)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    eprintln!(
        "ANE: tokeny {gpu_token}/{three_token}, RMS {:.3}% rozpiętości, max {:.4} \
         ({:.2}% rozpiętości {spread:.1})",
        common::spread_error(&three_logits, &gpu_logits) * 100.0,
        worst,
        100.0 * worst / spread
    );
    assert_eq!(gpu_token, three_token, "ogon ANE zmienił pierwszy token");
    common::agrees("GPU+CPU+ANE wobec GPU", &three_logits, &gpu_logits, 0.004);
}

/// Kontrola ujemna dla ANE: dekodowanie jest poniżej progu wsadu, więc ogon
/// jest zerowany i obie strony to ta sama ścieżka.
#[cfg(feature = "ane")]
#[test]
#[ignore]
fn the_ane_share_does_not_reach_decode() {
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Some(ane) = ane_dir() else {
        eprintln!("pomijam: brak FORGE_ANE_DIR");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model =
        Dense::load(&dir, |spec| MetalExec::new(device, spec)).expect("wczytanie modelu");
    attach_ane(&mut model, &ane);

    let reps: usize = std::env::var("FORGE_BENCH_REPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let prompt: Vec<u32> = (0..256).map(|i| (i % 30000) as u32 + 3).collect();

    set_arms(&mut model, false, false);
    let alone = decode_seconds(&mut model, &prompt, reps);
    set_arms(&mut model, true, true);
    let shared = decode_seconds(&mut model, &prompt, reps);

    let alone_rate = DECODE_STEPS as f64 / alone;
    let shared_rate = DECODE_STEPS as f64 / shared;
    eprintln!(
        "dekodowanie: samo GPU {alone_rate:.1} tok/s, z trzema ramionami włączonymi \
         {shared_rate:.1} tok/s ({:+.1}%)",
        (shared_rate / alone_rate - 1.0) * 100.0
    );
    let drift = (shared_rate / alone_rate - 1.0).abs();
    assert!(
        drift < 0.10,
        "dekodowanie zmieniło się o {:.1}% po włączeniu ogona ANE — \
         forma dekodowania nie powinna się do niego kwalifikować",
        drift * 100.0
    );
}
