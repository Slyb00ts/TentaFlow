// ===== File: cpu_encoder_vs_mlx.rs — host encoder against the mlx-whisper oracle =====
//
// Runs the whole path on this machine, with no GPU: a real MLX-quantized
// Whisper checkpoint is loaded through the production loader onto the CPU
// backend, the host reference encoder runs on it, and the result is compared
// against the encoder output of mlx-whisper itself.
//
// The checkpoint is small on purpose. The maths is identical to large-v3-turbo;
// only the dimensions differ, and a 32-layer f32 forward on a scalar host
// implementation would turn a gate into a coffee break.
//
// Fixture: tools/mlx-oracle/gen_tiny_whisper.py

use std::path::PathBuf;

use forge_hal::cpu::CpuDevice;
use forge_whisper::cpu_ref;
use forge_whisper::weights::WhisperWeights;

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny-mlx-whisper")
}

/// The same deterministic mel the generator feeds to MLX. Written out here
/// rather than stored: if the two formulas ever drift apart the test compares
/// different inputs and silently passes.
fn mel_input(n_mels: usize, n_in: usize) -> Vec<f32> {
    let mut out = vec![0f32; n_mels * n_in];
    for c in 0..n_mels {
        for t in 0..n_in {
            let v = ((c * 37 + t * 11) % 101) as f32 / 101.0 - 0.5;
            // The generator casts to f16 before feeding MLX, so the reference
            // must see the same value, not the f32 one.
            out[c * n_in + t] = half::f16::from_f32(v).to_f32();
        }
    }
    out
}

fn read_expected(path: &PathBuf) -> Option<(usize, usize, Vec<f32>)> {
    let blob = std::fs::read(path).ok()?;
    assert_eq!(&blob[0..4], b"WENC", "zły magic wyjścia enkodera");
    let at = |i: usize| u32::from_le_bytes(blob[i..i + 4].try_into().unwrap()) as usize;
    assert_eq!(at(4), 1, "wersja");
    let (ctx, state) = (at(8), at(12));
    let values = blob[16..]
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    Some((ctx, state, values))
}

#[test]
fn host_encoder_matches_mlx_whisper() {
    let dir = fixture_dir();
    let Some((ctx, state, want)) = read_expected(&dir.join("encoder_out.bin")) else {
        eprintln!("pomijam: brak fikstury — uruchom tools/mlx-oracle/gen_tiny_whisper.py");
        return;
    };

    let device = CpuDevice::new();
    let w = WhisperWeights::load(&*device, &dir).expect("wczytanie małego checkpointu MLX");
    assert_eq!(w.config.max_source_positions, ctx);
    assert_eq!(w.config.d_model, state);

    let mel = mel_input(w.config.num_mel_bins, ctx * 2);
    let got = cpu_ref::encode(&*device, &w, &mel).expect("forward enkodera na CPU");
    assert_eq!(got.len(), want.len());

    // Wagi są w f16 i przechodzą przez dekwantyzację 4-bitową, więc porównanie
    // jest w tolerancji, nie bit w bit.
    //
    // Miarą jest błąd względny w normie L2 całego wyjścia, a NIE największy błąd
    // pojedynczego elementu: przy wartościach rzędu 1e-3 obok wartości rzędu
    // 3e-1 ten drugi mierzy głównie to, jak blisko zera trafił element, i
    // odrzuciłby poprawną implementację. Kosinus dokłada się osobno, bo sam
    // błąd L2 przepuszcza wynik przeskalowany o stałą.
    let (rel_l2, cos) = compare(&got, &want);
    eprintln!("host vs mlx-whisper: rel_l2 {rel_l2:.3e}, kosinus {cos:.8}");
    assert!(
        cos > 0.9999,
        "kosinus {cos:.6}, rel_l2 {rel_l2:.3e} — to nie jest różnica zaokrągleń, \
         tylko inna matematyka"
    );
    assert!(
        rel_l2 <= 1.0e-2,
        "rel_l2 {rel_l2:.3e} wobec wyjścia mlx-whisper (kosinus {cos:.6})"
    );
}

/// Zwraca (rel_l2, kosinus) dla dwóch wektorów tej samej długości.
fn compare(got: &[f32], want: &[f32]) -> (f64, f64) {
    let (mut diff, mut norm, mut dot, mut na, mut nb) = (0f64, 0f64, 0f64, 0f64, 0f64);
    for (g, v) in got.iter().zip(want) {
        let (g, v) = (*g as f64, *v as f64);
        diff += (g - v) * (g - v);
        norm += v * v;
        dot += g * v;
        na += g * g;
        nb += v * v;
    }
    (
        (diff / norm.max(1e-30)).sqrt(),
        dot / (na.sqrt() * nb.sqrt()).max(1e-30),
    )
}

#[test]
fn a_transposed_mel_is_caught_by_the_comparison() {
    // Kontrola samego testu. Zielony wynik porównania znaczy coś tylko wtedy,
    // gdy porównanie w ogóle potrafi zobaczyć błąd — a przestawienie osi mel to
    // dokładnie ta klasa pomyłki, którą ten checkpoint już raz wymusił przy
    // splotach. Wymiary są tu kwadratowe (16 × 16), więc transpozycja jest
    // dokładnie zdefiniowana.
    let dir = fixture_dir();
    let Some((ctx, _state, want)) = read_expected(&dir.join("encoder_out.bin")) else {
        eprintln!("pomijam: brak fikstury");
        return;
    };
    let device = CpuDevice::new();
    let w = WhisperWeights::load(&*device, &dir).unwrap();

    let mels = w.config.num_mel_bins;
    let n_in = ctx * 2;
    assert_eq!(mels, n_in, "kontrola zakłada kwadratowe wejście");
    let good = mel_input(mels, n_in);
    let mut transposed = vec![0f32; good.len()];
    for c in 0..mels {
        for t in 0..n_in {
            transposed[c * n_in + t] = good[t * n_in + c];
        }
    }

    let got = cpu_ref::encode(&*device, &w, &transposed).unwrap();
    let (rel_l2, cos) = compare(&got, &want);
    assert!(
        cos < 0.999 || rel_l2 > 1.0e-2,
        "przestawione wejście dało kosinus {cos:.6} i rel_l2 {rel_l2:.3e} — \
         porównanie nie odróżnia wejść"
    );
}
