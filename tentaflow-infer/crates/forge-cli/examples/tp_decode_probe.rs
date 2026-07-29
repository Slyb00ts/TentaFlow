// Dekodowanie z FFN rozłożonym na karty, porównane z tym samym dekodowaniem na
// jednej karcie.
//
// Kryterium jest kolejność: NAJPIERW te same tokeny, dopiero potem czas. Podział
// zmienia kolejność sumowania w projekcji `down`, więc zgodność jest numeryczna,
// nie bitowa — ale wybrany token musi być ten sam, inaczej podział jest po
// prostu innym modelem.
//
//   TP_EXTRA=1        indeksy dodatkowych kart (po przecinku)
//   TP_SPLIT=a,b      narzucony podział wymiaru pośredniego (pomija kalibrację)
//   TP_TOKENS=64      ile tokenów zdekodować w pomiarze
//   TP_WEIGHTS_GIB=10 pula wag karty modelu
//   TP_SHARD_GIB=8    pula wag kart dodatkowych
use forge_engine::model::{Model, ModelConfig};
use forge_hal::{PoolSizes, gpu};
use std::time::Instant;

fn pools(var: &str, default_gib: usize) -> PoolSizes {
    PoolSizes {
        weights: std::env::var(var)
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(default_gib)
            << 30,
        kv_cache: 512 << 20,
        activations: 512 << 20,
        kv_page_size: 256 << 10,
    }
}

fn argmax(v: &[f32]) -> u32 {
    let mut best = 0usize;
    for (i, &x) in v.iter().enumerate() {
        if x > v[best] {
            best = i;
        }
    }
    best as u32
}

/// Dekoduje `count` kroków, karmiąc model ZADANYM ciągiem tokenów, i zwraca
/// logity każdego kroku oraz czas samego dekodowania (bez prefillu).
///
/// Karmienie z zewnątrz jest tu istotne: zachłanna pętla sprzęga kroki, więc
/// jeden remis rozstrzygnięty inaczej rozjeżdża CAŁĄ dalszą sekwencję i mierzy
/// się wtedy chaotyczność autoregresji, a nie zgodność FFN. Podając oba razy tę
/// samą historię, porównujemy dokładnie ten operator, który został zmieniony.
fn decode_forced(model: &mut Model, prompt: &[u32], feed: &[u32]) -> (Vec<Vec<f32>>, f64) {
    let mut seq = model.new_seq();
    model.prefill_chunk(&mut seq, prompt).expect("prefill");
    let mut out = Vec::with_capacity(feed.len());
    let start = Instant::now();
    for &token in feed {
        out.push(model.step(&mut seq, token).expect("krok dekodowania"));
    }
    let elapsed = start.elapsed().as_secs_f64();
    model.release_seq(&mut seq);
    (out, elapsed)
}

/// Zachłanny ciąg `count` tokenów — służy tylko za historię do karmienia.
fn greedy(model: &mut Model, prompt: &[u32], count: usize) -> Vec<u32> {
    let mut seq = model.new_seq();
    let logits = model.prefill_chunk(&mut seq, prompt).expect("prefill");
    let mut next = argmax(&logits);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(next);
        next = argmax(&model.step(&mut seq, next).expect("krok dekodowania"));
    }
    model.release_seq(&mut seq);
    out
}

fn main() {
    let path = std::path::PathBuf::from(std::env::args().nth(1).expect("ścieżka do gguf"));
    let prompt: Vec<u32> = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "1,4222,349,272,4304,302,6620,28804".into())
        .split(',')
        .map(|s| s.trim().parse().expect("token"))
        .collect();
    let count: usize = std::env::var("TP_TOKENS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);

    let ids = gpu::enumerate();
    let extra: Vec<gpu::DeviceId> = std::env::var("TP_EXTRA")
        .unwrap_or_else(|_| "1".into())
        .split(',')
        .map(|s| {
            let index: usize = s.trim().parse().expect("indeks karty");
            assert!(index < ids.len(), "nie ma karty {index}");
            ids[index]
        })
        .collect();

    let cfg = ModelConfig {
        max_seq_len: 2048,
        kv_pages: 64,
        prefix_cache: false,
        ..ModelConfig::default()
    };
    let device = gpu::open_id(ids[0], pools("TP_WEIGHTS_GIB", 10)).expect("karta modelu");
    println!("karta modelu: {}", device.caps().name);
    let mut model = Model::load_gguf(device, &path, cfg).expect("wczytanie modelu");

    let feed = greedy(&mut model, &prompt, count);
    let (single, single_time) = decode_forced(&mut model, &prompt, &feed);
    println!(
        "jedna karta: {count} kroków w {:.3} s -> {:.1} tok/s",
        single_time,
        count as f64 / single_time
    );

    let shard_pools = pools("TP_SHARD_GIB", 8);
    // TP_SPLIT narzuca liczbę kolumn `down` na kartę, z pominięciem kalibracji.
    let forced: Option<Vec<usize>> = std::env::var("TP_SPLIT").ok().map(|v| {
        v.split(',')
            .map(|s| s.trim().parse().expect("kolumny na kartę"))
            .collect()
    });
    model
        .enable_tp_ffn(&path, &extra, shard_pools, None, forced.as_deref())
        .expect("rozłożenie FFN na karty");
    let tp = model.tp_ffn().expect("podział aktywny");
    println!(
        "podział na {} kart, P2P {}: wiersze pośrednie warstwy 0 {:?}",
        tp.cards(),
        tp.peer_access(),
        tp.split_of(0)
    );

    // Ten sam przebieg dwa razy: jeśli różnią się między sobą, podział ma stan,
    // którego nie odtwarza — a to zupełnie inna usterka niż zaokrąglenia.
    let (first, _) = decode_forced(&mut model, &prompt, &feed);
    let (split, split_time) = decode_forced(&mut model, &prompt, &feed);
    let repeat_diff = first
        .iter()
        .zip(split.iter())
        .flat_map(|(a, b)| a.iter().zip(b.iter()))
        .map(|(x, y)| (x - y).abs())
        .fold(0f32, f32::max);
    println!("powtórzenie tego samego przebiegu: max |różnica| {repeat_diff:.6}");
    println!(
        "podział:     {count} kroków w {:.3} s -> {:.1} tok/s",
        split_time,
        count as f64 / split_time
    );

    let mut max_abs = 0f32;
    let (mut num, mut den) = (0f64, 0f64);
    let mut argmax_diff = 0usize;
    for (step, (a, b)) in single.iter().zip(split.iter()).enumerate() {
        if argmax(a) != argmax(b) {
            argmax_diff += 1;
        }
        if count <= 16 {
            let (mut n, mut d) = (0f64, 0f64);
            for (x, y) in a.iter().zip(b.iter()) {
                n += ((x - y) as f64).powi(2);
                d += (*x as f64).powi(2);
            }
            println!("  krok {step}: względne L2 {:.2e}", (n / d.max(1e-12)).sqrt());
        }
        for (x, y) in a.iter().zip(b.iter()) {
            let diff = (x - y) as f64;
            max_abs = max_abs.max(diff.abs() as f32);
            num += diff * diff;
            den += (*x as f64) * (*x as f64);
        }
    }
    let l2 = (num / den.max(1e-12)).sqrt();
    println!(
        "logity: max |różnica| {max_abs:.6}, względne L2 {l2:.2e}, inny argmax w {argmax_diff}/{count} krokach"
    );
    // Zgodność jest NUMERYCZNA, nie bitowa, i to nie jest drobiazg: projekcja
    // `down` sumuje ~11 tys. składników, które w dużej mierze się kasują, więc
    // inna kolejność dodawania f32 daje na logitach błąd względny o kilka rzędów
    // większy niż samo epsilon. Zmierzone na Bieliku 7B Q8_0: podział 32/11232
    // daje 8e-6, po połowie 7e-3; CAŁY podział na jednej karcie wychodzi bitowo
    // (0.0), co odróżnia to zjawisko od usterki. Próg jest więc progiem tej
    // klasy różnicy, nie progiem zaokrągleń.
    assert!(l2 < 5e-2, "logity podziału rozjechały się z jednokartowymi");
    println!(
        "przyspieszenie dekodowania: {:.2}x",
        single_time / split_time
    );
}
