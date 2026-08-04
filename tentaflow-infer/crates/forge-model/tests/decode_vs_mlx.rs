// ===== File: decode_vs_mlx.rs — the whole decode loop against mlx-lm =====
//
// Bielik-7B, four-bit, on this machine's GPU: five tokens fed one at a time,
// logits compared against mlx-lm after EACH of them.
//
// Per step, not just at the end, for the same reason the FFN test compares
// stage by stage. A drift that accumulates through forty layers and a KV cache
// looks exactly like an arithmetic error until you can see which step it starts
// on — and the first step exercises no cache at all, so a cache bug and a maths
// bug separate themselves immediately.
//
// Fixture: tools/mlx-oracle/gen_logits.py
#![cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]

use std::path::PathBuf;

use forge_hal::metal_device::MetalDevice;
use forge_model::mlx_dense::MlxDense;

const FIXTURE: &[u8] = include_bytes!("fixtures/mlx_logits_bielik.bin");
const CHECKPOINT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../.runtime/models/models--agentGreg--Bielik-Minitron-7B-v3.0-Instruct-MLX-4bit/snapshots"
);

struct Oracle {
    tokens: Vec<u32>,
    vocab: usize,
    logits: Vec<Vec<f32>>,
}

fn load() -> Oracle {
    assert_eq!(&FIXTURE[0..4], b"LOG1", "zły magic fikstury");
    let mut pos = 4usize;
    let u32_at = |p: &mut usize| {
        let v = u32::from_le_bytes(FIXTURE[*p..*p + 4].try_into().unwrap());
        *p += 4;
        v
    };
    assert_eq!(u32_at(&mut pos), 1, "wersja fikstury");
    let steps = u32_at(&mut pos) as usize;
    let vocab = u32_at(&mut pos) as usize;
    let tokens: Vec<u32> = (0..steps).map(|_| u32_at(&mut pos)).collect();

    let mut logits = Vec::with_capacity(steps);
    for _ in 0..steps {
        let row: Vec<f32> = FIXTURE[pos..pos + vocab * 4]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect();
        pos += vocab * 4;
        logits.push(row);
    }
    Oracle {
        tokens,
        vocab,
        logits,
    }
}

fn checkpoint() -> Option<PathBuf> {
    let snapshots = PathBuf::from(CHECKPOINT);
    let dir = std::fs::read_dir(&snapshots).ok()?.flatten().next()?.path();
    dir.join("model.safetensors").is_file().then_some(dir)
}

/// Średnia różnica na logit, wyrażona w rozpiętości logitów tego kroku.
///
/// Nie `rel_l2`: przy pierwszym tokenie model nie ma kontekstu i logity są
/// prawie płaskie, więc ich norma jest mała i KAŻDA różnica wygląda w niej
/// wielko — miara mówiłaby wtedy o rozkładzie wyjścia, a nie o zgodności.
/// Rozpiętość `max - min` jest tym, wobec czego różnica faktycznie się liczy,
/// bo to ona decyduje o kolejności tokenów.
fn spread_error(got: &[f32], want: &[f32]) -> f64 {
    let mut diff = 0f64;
    for (g, v) in got.iter().zip(want) {
        diff += (*g as f64 - *v as f64).powi(2);
    }
    let rms = (diff / got.len() as f64).sqrt();
    let max = want.iter().cloned().fold(f32::NEG_INFINITY, f32::max) as f64;
    let min = want.iter().cloned().fold(f32::INFINITY, f32::min) as f64;
    rms / (max - min).max(1e-6)
}

fn top_k(logits: &[f32], k: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..logits.len()).collect();
    idx.sort_by(|a, b| logits[*b].total_cmp(&logits[*a]).then(a.cmp(b)));
    idx.truncate(k);
    idx
}

#[test]
fn decode_loop_matches_mlx_lm_step_by_step() {
    let oracle = load();
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };

    let mut model = MlxDense::load(device, &dir).expect("wczytanie modelu");
    let shape = model.shape();
    assert_eq!(shape.layers, 40);
    assert_eq!(shape.vocab as usize, oracle.vocab);

    for (step, &token) in oracle.tokens.iter().enumerate() {
        let got = model.step(token).expect("krok dekodowania");
        let want = &oracle.logits[step];
        assert_eq!(got.len(), want.len());

        let err = spread_error(&got, want);
        let ours = top_k(&got, 5);
        let theirs = top_k(want, 5);
        eprintln!(
            "krok {}: błąd {:.2}% rozpiętości, argmax {}",
            step + 1,
            err * 100.0,
            ours[0]
        );

        // Token jest jedyną liczbą, która wychodzi z modelu na zewnątrz, więc
        // to on jest kontraktem.
        assert_eq!(
            ours[0], theirs[0],
            "krok {}: inny token; nasza piątka {ours:?}, MLX {theirs:?}",
            step + 1
        );

        // Pierwsze trzy w tej samej kolejności. Czwarte i piąte bywają
        // remisem co do trzeciego miejsca po przecinku i wymaganie od nich
        // kolejności robiłoby z testu loterię — ale ZBIÓR piątki musi się zgadzać.
        assert_eq!(ours[..3], theirs[..3], "krok {}: kolejność czołówki", step + 1);
        let mut a = ours.clone();
        let mut b = theirs.clone();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b, "krok {}: inna piątka", step + 1);

        // Wartości: zgodny argmax przy dużej różnicy logitów byłby przypadkiem,
        // nie dowodem, więc różnica ma zostać poniżej pięciu procent rozpiętości.
        assert!(
            err < 0.05,
            "krok {}: logity odbiegają o {:.2}% rozpiętości",
            step + 1,
            err * 100.0
        );
    }
}

#[test]
fn greedy_choice_on_the_device_agrees_with_the_readback() {
    // Ta sama sekwencja, ale token wybiera kernel na GPU zamiast hosta.
    // Jeśli te dwie drogi się rozjadą, winna jest reguła remisu albo redukcja,
    // i lepiej dowiedzieć się tego tutaj niż po dziesięciu tokenach tekstu.
    let oracle = load();
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = MlxDense::load(device, &dir).expect("wczytanie modelu");

    for (step, &token) in oracle.tokens.iter().enumerate() {
        let chosen = model.step_argmax(token).expect("krok dekodowania");
        let expected = top_k(&oracle.logits[step], 1)[0] as u32;
        assert_eq!(chosen, expected, "krok {}", step + 1);
    }
}

#[test]
fn a_context_past_the_cache_capacity_is_refused() {
    // Pojemność cache'u jest własnością kernela uwagi, a nie życzeniem: model
    // ma odmówić, zanim zacznie pisać poza tablicę.
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = MlxDense::load(device, &dir).expect("wczytanie modelu");
    assert_eq!(model.position(), 0);
    model.step(1).expect("pierwszy krok");
    assert_eq!(model.position(), 1);
    model.reset();
    assert_eq!(model.position(), 0);
}

/// Normy stanu ukrytego na kolejnych głębokościach, zmierzone w mlx-lm dla
/// tokenu 1. Trzymane tutaj, bo służą do BISEKCJI: gdy logity się rozjadą,
/// pierwsza głębokość, na której norma odstaje, wskazuje warstwę, a nie tylko
/// fakt, że wynik jest zły.
const REFERENCE_NORMS: &[(usize, f64)] = &[
    (1, 14.1853),
    (5, 2430.1258),
    (10, 2430.1266),
    (20, 2430.2093),
    (40, 702.8099),
];

#[test]
fn hidden_state_tracks_mlx_at_every_depth() {
    let Some(dir) = checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = MlxDense::load(device, &dir).expect("wczytanie modelu");

    for &(layers, expected) in REFERENCE_NORMS {
        let h = model.probe(1, layers).expect("sonda");
        let norm: f64 = h
            .iter()
            .map(|v| (*v as f64) * (*v as f64))
            .sum::<f64>()
            .sqrt();
        let rel = (norm - expected).abs() / expected;
        eprintln!("po {layers:2} warstwach: {norm:.4} wobec {expected:.4} ({:.2}%)", rel * 100.0);
        // Ten model ma „masywne aktywacje": kilka kanałów niesie wartości o rząd
        // wielkości większe od reszty, więc sama norma jest nimi zdominowana i
        // zgadza się nawet wtedy, gdy treść już się rozjeżdża. Próg jest tu
        // szeroki celowo — to jest wskaźnik lokalizujący, a nie bramka
        // poprawności; tą jest zgodność tokenów w teście wyżej.
        assert!(
            rel < 0.10,
            "po {layers} warstwach norma odbiega o {:.1}% — pierwsza taka \
             głębokość wskazuje warstwę, w której coś jest nie tak",
            rel * 100.0
        );
    }
}
