// ===== File: gguf_loads.rs — ten sam model, drugi format =====
//
// Bielik istnieje w dwóch postaciach: eksport MLX (affine 4-bit, grupa 64,
// skale bf16) i GGUF Q4_K_M (Q4_K na większości wag, Q6_K na attn_v, ffn_down
// i głowie). To ten sam model, więc jedyne, co ten test sprawdza, to czy droga
// wejścia jest naprawdę wspólna — czy model wczyta jedno i drugie bez ani
// jednej gałęzi po swojej stronie.
#![cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]

use std::path::{Path, PathBuf};

use forge_hal::metal_device::MetalDevice;
use forge_kernels::{HostExec, MetalExec};
use forge_model::dense::{Dense, Feed};

/// Slot cache'u tych testów. Jedna sekwencja, więc zawsze ten sam.
const SLOT: usize = 0;

/// Model plus wykonawca. To jedyne miejsce w teście, które wie, co liczy —
/// `Dense` dostaje wykonawcę jako wytwórnię i nigdy nie pyta, czym on jest.
fn open(device: std::sync::Arc<MetalDevice>, path: &std::path::Path) -> Dense<MetalExec> {
    Dense::load(path, |spec| MetalExec::new(device, spec)).expect("wczytanie modelu")
}

const GGUF: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../.runtime/models/bielik-minitron-7b-v3-gguf/minitron-Bielik-7B-v3.0-Instruct-Q4_K_M.gguf"
);

fn gguf() -> Option<PathBuf> {
    let p = PathBuf::from(GGUF);
    p.is_file().then_some(p)
}

#[test]
#[ignore]
fn the_gguf_build_of_the_same_model_loads_and_generates() {
    let Some(path) = gguf() else {
        eprintln!("pomijam: brak pliku GGUF");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };

    let mut model = open(device, Path::new(&path));
    let shape = model.shape();
    assert_eq!(shape.layers, 40, "inna liczba warstw niż w MLX");
    assert_eq!(shape.hidden, 4096);
    assert_eq!(shape.kv_heads * shape.head_dim, 1024);

    // Prompt przechodzi przez prefill (forma blokowa i macierzowa) oraz
    // dekodowanie (wektorowa), czyli przez wszystkie trzy formy na obu
    // szerokościach kodu naraz.
    let prompt: Vec<u32> = vec![1, 4234, 8123, 302, 15];
    let first = model.prefill(SLOT, &prompt).expect("prefill");
    let mut out = vec![first];
    for _ in 0..3 {
        let token = *out.last().unwrap();
        out.push(model.decode(&[Feed { slot: SLOT, token }]).expect("krok")[0]);
    }
    eprintln!("wygenerowane tokeny: {out:?}");
    assert!(
        out.iter().all(|t| *t < shape.vocab),
        "token poza słownikiem: {out:?}"
    );
    // Model zwracający w kółko ten sam token przeszedł ścieżkę, ale nie
    // policzył — a to wygląda identycznie w statystykach.
    assert!(
        out.iter().collect::<std::collections::HashSet<_>>().len() > 1,
        "wszystkie tokeny identyczne, model nie liczy: {out:?}"
    );
}

#[test]
#[ignore]
fn the_metal_gguf_path_agrees_with_the_host_reference() {
    let Some(path) = gguf() else {
        eprintln!("pomijam: brak pliku GGUF");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut metal = open(device, Path::new(&path));
    let mut host = Dense::load(Path::new(&path), HostExec::new).expect("wczytanie wzorca");
    let prompt = vec![1, 4234, 8123, 302, 15];

    let metal_first = metal.prefill(SLOT, &prompt).expect("Metal prefill");
    let host_first = host.prefill(SLOT, &prompt).expect("host prefill");
    assert_eq!(metal_first, host_first, "GGUF wybrał inny pierwszy token");

    let metal_logits = metal.logits(0).expect("Metal logity");
    let host_logits = host.logits(0).expect("host logity");
    let max_abs = metal_logits
        .iter()
        .zip(&host_logits)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(max_abs < 0.25, "GGUF Metal odjechał od wzorca o {max_abs}");
}

#[test]
#[ignore]
fn the_gguf_cpu_share_agrees_with_gpu_only() {
    let Some(path) = gguf() else {
        eprintln!("pomijam: brak pliku GGUF");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = open(device, Path::new(&path));
    let seed = [1, 4234, 8123, 302, 15];
    let prompt: Vec<u32> = seed.iter().copied().cycle().take(256).collect();

    model.exec_mut().set_cpu_share(false);
    model.reset(SLOT).expect("reset GPU-only");
    let gpu_token = model.prefill(SLOT, &prompt).expect("prefill GPU-only");
    let gpu_logits = model.logits(0).expect("logity GPU-only");

    model.exec_mut().set_cpu_share(true);
    model.reset(SLOT).expect("reset CPU-share");
    let shared_token = model.prefill(SLOT, &prompt).expect("prefill CPU-share");
    let shared_logits = model.logits(0).expect("logity CPU-share");

    let (worst, sum_sq, norm_sq, span) = gpu_logits.iter().zip(&shared_logits).fold(
        (0.0f32, 0.0f64, 0.0f64, (f32::MIN, f32::MAX)),
        |(worst, sum_sq, norm_sq, (max, min)), (&gpu, &shared)| {
            (
                worst.max((gpu - shared).abs()),
                sum_sq + f64::from(gpu - shared).powi(2),
                norm_sq + f64::from(gpu).powi(2),
                (max.max(gpu), min.min(gpu)),
            )
        },
    );
    let span = span.0 - span.1;
    let rel = (sum_sq / norm_sq).sqrt();
    eprintln!(
        "GGUF CPU-share: tokens {gpu_token}/{shared_token}, max {worst:.4}, rel L2 {rel:.2e}, span {span:.1}"
    );
    assert_eq!(gpu_token, shared_token, "CPU-share zmienił pierwszy token");
    assert!(
        worst < 0.004 * span,
        "GGUF CPU-share różni się od GPU-only o {:.2}% rozpiętości",
        100.0 * worst / span
    );
}

#[test]
#[ignore]
fn the_gguf_cpu_share_speed_is_measured_interleaved() {
    let Some(path) = gguf() else {
        eprintln!("pomijam: brak pliku GGUF");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = open(device, Path::new(&path));
    let prompt: Vec<u32> = [1, 4234, 8123, 302, 15].into_iter().cycle().take(1024).collect();
    let mut samples = [Vec::new(), Vec::new()];

    for round in 0..4 {
        let order = if round % 2 == 0 { [false, true] } else { [true, false] };
        for cpu_share in order {
            model.exec_mut().set_cpu_share(cpu_share);
            model.reset(SLOT).expect("reset");
            let start = std::time::Instant::now();
            model.prefill(SLOT, &prompt).expect("prefill");
            if round > 0 {
                samples[usize::from(cpu_share)].push(start.elapsed().as_secs_f64());
            }
        }
    }
    for values in &mut samples {
        values.sort_by(f64::total_cmp);
    }
    let off = samples[0][samples[0].len() / 2];
    let on = samples[1][samples[1].len() / 2];
    eprintln!(
        "GGUF CPU-share speed: GPU-only {:.1} tok/s, GPU+CPU {:.1} tok/s ({:+.1}%)",
        1024.0 / off,
        1024.0 / on,
        (off / on - 1.0) * 100.0
    );
}

/// Ten sam model w dwóch formatach, na tej samej maszynie, w jednym przebiegu.
/// Kolejność jest stała, żeby oba pomiary miały identyczne warunki procesu.
#[test]
#[ignore]
fn the_two_formats_of_the_same_model_side_by_side() {
    let Some(gguf_path) = gguf() else {
        eprintln!("pomijam: brak pliku GGUF");
        return;
    };
    let mlx_path = {
        let snapshots = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../.runtime/models/models--agentGreg--Bielik-Minitron-7B-v3.0-Instruct-MLX-4bit/snapshots"
        ));
        let Some(dir) = std::fs::read_dir(&snapshots)
            .ok()
            .and_then(|mut d| d.next())
        else {
            eprintln!("pomijam: brak checkpointu MLX");
            return;
        };
        dir.expect("wpis katalogu").path()
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };

    // Tokenizer bierzemy Z PLIKU GGUF — to ten sam, który niesie model, więc
    // porównanie nie zależy od tego, czy obok leży `tokenizer.json`.
    let gg = forge_formats::gguf::Gguf::open(&gguf_path).expect("gguf");
    let vocab = forge_tokenize::gguf_vocab(&gg).expect("słownik z gguf");
    let tokenizer = forge_tokenize::Tokenizer::from_gguf_vocab(&vocab).expect("tokenizer");
    // Prompt DŁUGI, nie ten siedmiotokenowy, którym zaczyna się rozmowa.
    // Prefill jest ograniczony obliczeniami i jego przepustowość rośnie z
    // liczbą tokenów w kaflu: na siedmiu tokenach mierzy się narzut stały, a
    // wynik w tok/s mówi wtedy o starcie, nie o ścieżce. Domyślnie tyle, ile
    // wynosi kafel prefillu, żeby pomiar objął formę macierzową i podział
    // pracy z CPU — czyli to, co w prefillu naprawdę liczy.
    //
    // Tekst CIĄGŁY, a nie jedno zdanie powtórzone sto razy. Kilkaset powtórzeń
    // tych samych siedmiu tokenów to wejście, na które KAŻDY model odpowiada
    // zapętleniem — i wtedy bramka „to ma być język" mierzy zdegenerowany
    // prompt, a nie ścieżkę liczenia.
    const TEKST: &str = "Wisła jest najdłuższą rzeką Polski i płynie z południa \
        na północ, od Baraniej Góry aż po Zatokę Gdańską. Po drodze mija Kraków, \
        Sandomierz, Warszawę i Toruń, a jej dorzecze obejmuje niemal połowę \
        powierzchni kraju. Przez wieki była główną drogą handlową: spławiano nią \
        zboże i drewno do Gdańska, skąd towary trafiały na rynki całej Europy. \
        Dzisiaj żegluga ma znaczenie głównie turystyczne, ale rzeka nadal \
        kształtuje krajobraz, rolnictwo i miasta, które nad nią wyrosły. \
        Wiosenne roztopy potrafią podnieść poziom wody o kilka metrów, dlatego \
        wzdłuż brzegów usypano wały, a w dolinie zachowano tereny zalewowe. \
        Ochrona przyrody i bezpieczeństwo powodziowe bywają tu w sprzeczności, \
        którą trzeba rozstrzygać osobno dla każdego odcinka. ";
    let want: usize = std::env::var("FORGE_BENCH_PROMPT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024);
    let seed = tokenizer.encode(TEKST, false).expect("encode");
    let mut prompt = vec![1];
    while prompt.len() < want {
        prompt.extend_from_slice(&seed);
    }
    prompt.truncate(want);

    let row = |label: &str, path: &Path| {
        let mut m = open(device.clone(), path);
        let cpu_share = std::env::var("FORGE_CPU_SHARE")
            .map(|value| value != "0")
            .unwrap_or(true);
        m.exec_mut().set_cpu_share(cpu_share);
        // Rozgrzewka, potem mediana z trzech — jak w pozostałych pomiarach.
        let mut prefills = Vec::new();
        let mut first = 0;
        for r in 0..4 {
            m.reset(SLOT).expect("reset");
            let t = std::time::Instant::now();
            first = m.prefill(SLOT, &prompt).expect("prefill");
            if r > 0 {
                prefills.push(t.elapsed().as_secs_f64());
            }
        }
        prefills.sort_by(f64::total_cmp);
        let prefill = prefills[prefills.len() / 2];

        let t = std::time::Instant::now();
        let mut tok = first;
        let mut out = vec![first];
        for _ in 0..15 {
            tok = m
                .decode(&[Feed {
                    slot: SLOT,
                    token: tok,
                }])
                .expect("krok")[0];
            out.push(tok);
        }
        let decode = t.elapsed().as_secs_f64();
        eprintln!(
            "{label:6}: prefill {} tok w {:.3} s ({:.1} tok/s), dekodowanie {:.1} tok/s",
            prompt.len(),
            prefill,
            prompt.len() as f64 / prefill,
            15.0 / decode
        );
        eprintln!(
            "        {:?}",
            tokenizer.decode(&out, true).unwrap_or_default()
        );
        out
    };

    let mlx = row("MLX", &mlx_path);
    let gguf_out = row("GGUF", &gguf_path);

    // Dwie NIEZALEŻNE kwantyzacje tego samego modelu różnią się na wadze o
    // około 11% — zmierzone — więc po czterdziestu warstwach mają pełne prawo
    // pisać co innego. Zgodność tokenów NIE jest tu kontraktem.
    //
    // Kontraktem jest to, że oba piszą JĘZYKIEM. Bez permutacji wierszy Q i K
    // ścieżka GGUF dawała „mušić,|\n\n\n\n /******/" — płynne śmieci, które
    // przechodzą każdy test na „tokeny są różne i w słowniku".
    for (label, out) in [("MLX", &mlx), ("GGUF", &gguf_out)] {
        let text = tokenizer.decode(out, true).expect("detokenizacja");
        let letters = text
            .chars()
            .filter(|c| c.is_alphabetic() || *c == ' ')
            .count();
        assert!(
            letters * 10 >= text.chars().count() * 6,
            "{label}: {letters} liter na {} znaków — to nie jest tekst: {text:?}",
            text.chars().count()
        );
    }
}
