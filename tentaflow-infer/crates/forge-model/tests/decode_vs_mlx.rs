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

mod common;

use forge_hal::metal_device::MetalDevice;
use forge_kernels::MetalExec;
use forge_model::dense::{Dense, Feed};

/// Slot cache'u tych testów. Jedna sekwencja, więc zawsze ten sam.
const SLOT: usize = 0;

/// Model plus wykonawca. To jedyne miejsce w teście, które wie, co liczy —
/// `Dense` dostaje wykonawcę jako wytwórnię i nigdy nie pyta, czym on jest.
fn open(device: std::sync::Arc<MetalDevice>, path: &std::path::Path) -> Dense<MetalExec> {
    Dense::load(path, |spec| MetalExec::new(device, spec)).expect("wczytanie modelu")
}


#[test]
fn decode_loop_matches_mlx_lm_step_by_step() {
    let oracle = common::load();
    let Some(dir) = common::checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };

    let mut model = open(device, &dir);
    let shape = model.shape();
    assert_eq!(shape.layers, 40);
    assert_eq!(shape.vocab as usize, oracle.vocab);

    for (step, &token) in oracle.tokens.iter().enumerate() {
        model
            .decode(&[Feed { slot: SLOT, token }])
            .expect("krok dekodowania");
        let got = model.logits(0).expect("logity");
        let want = &oracle.logits[step];
        assert_eq!(got.len(), want.len());

        let err = common::spread_error(&got, want);
        let ours = common::top_k(&got, 5);
        let theirs = common::top_k(want, 5);
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
    let oracle = common::load();
    let Some(dir) = common::checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = open(device, &dir);

    for (step, &token) in oracle.tokens.iter().enumerate() {
        let chosen = model
            .decode(&[Feed { slot: SLOT, token }])
            .expect("krok dekodowania")[0];
        let expected = common::top_k(&oracle.logits[step], 1)[0] as u32;
        assert_eq!(chosen, expected, "krok {}", step + 1);
    }
}

#[test]
fn a_context_past_the_cache_capacity_is_refused() {
    // Pojemność cache'u jest własnością kernela uwagi, a nie życzeniem: model
    // ma odmówić, zanim zacznie pisać poza tablicę.
    let Some(dir) = common::checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = open(device, &dir);
    assert_eq!(model.position(SLOT).expect("pozycja"), 0);
    model
        .decode(&[Feed { slot: SLOT, token: 1 }])
        .expect("pierwszy krok");
    assert_eq!(model.position(SLOT).expect("pozycja"), 1);
    model.reset(SLOT).expect("reset");
    assert_eq!(model.position(SLOT).expect("pozycja"), 0);
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
    let Some(dir) = common::checkpoint() else {
        eprintln!("pomijam: brak checkpointu Bielika");
        return;
    };
    let Ok(device) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let mut model = open(device, &dir);

    for &(layers, expected) in REFERENCE_NORMS {
        let h = model.probe(SLOT, 1, layers).expect("sonda");
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
