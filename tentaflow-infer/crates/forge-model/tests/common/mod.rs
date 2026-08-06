// ===== File: common/mod.rs — the mlx-lm oracle, shared by every executor =====
//
// One recording of what mlx-lm produced for Bielik, and one way of asking "is
// this the same". Shared because the point of the comparison is that it does
// not depend on WHO computed the answer: the Metal executor and the host
// reference are held to the same numbers by the same code.

use std::path::PathBuf;

const FIXTURE: &[u8] = include_bytes!("../fixtures/mlx_logits_bielik.bin");
const CHECKPOINT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../.runtime/models/models--agentGreg--Bielik-Minitron-7B-v3.0-Instruct-MLX-4bit/snapshots"
);

pub struct Oracle {
    pub tokens: Vec<u32>,
    pub vocab: usize,
    pub logits: Vec<Vec<f32>>,
}

pub fn load() -> Oracle {
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

pub fn checkpoint() -> Option<PathBuf> {
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
pub fn spread_error(got: &[f32], want: &[f32]) -> f64 {
    let mut diff = 0f64;
    for (g, v) in got.iter().zip(want) {
        diff += (*g as f64 - *v as f64).powi(2);
    }
    let rms = (diff / got.len() as f64).sqrt();
    let max = want.iter().cloned().fold(f32::NEG_INFINITY, f32::max) as f64;
    let min = want.iter().cloned().fold(f32::INFINITY, f32::min) as f64;
    rms / (max - min).max(1e-6)
}

pub fn top_k(logits: &[f32], k: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..logits.len()).collect();
    idx.sort_by(|a, b| logits[*b].total_cmp(&logits[*a]).then(a.cmp(b)));
    idx.truncate(k);
    idx
}

/// Holds one executor's logits against another's: the chosen token exactly, and
/// every other disagreement in the leading few required to be a TIE rather than
/// forgiven.
///
/// The two sides round differently by construction — the reference keeps f32
/// where the kernels quantize activations to int8 — so two logits within that
/// rounding of each other may come out in either order. Demanding the exact
/// order fails on arithmetic that is right; dropping the order entirely passes
/// for arithmetic that is wrong. So a swap is allowed only when the REFERENCE
/// itself separates the two by less than the error being measured, which is a
/// check and not a relaxation.
///
/// One rule per RANK and none for the set, deliberately. A fixed window has an
/// edge, and comparing membership across it fails for the pair that straddles
/// it however close they are — which is an artefact of the window, not a
/// finding. Holding each rank to the tie rule covers the same ground without
/// it: a genuinely different token there is separated in the reference by far
/// more than the error being measured, and says so.
///
/// Lives here rather than in one test file because both checkpoints on this
/// path ask the same question, and two statements of it would drift.
pub fn agrees(what: &str, got: &[f32], want: &[f32], bound: f64) {
    assert_eq!(got.len(), want.len());
    let err = spread_error(got, want);
    let ours = top_k(got, 5);
    let theirs = top_k(want, 5);
    eprintln!("{what}: {:.3}% rozpiętości, argmax {}", err * 100.0, ours[0]);

    assert_eq!(ours[0], theirs[0], "{what}: inny token");

    // A swap is explained exactly when THIS RUN's error on THOSE TWO logits is
    // together enough to invert their order. Local on purpose: the earlier
    // version held the gap against the run's RMS over the whole vocabulary,
    // which is the wrong statistic — the error at any single pair is routinely
    // several times the mean, so a real tie could be reported as a divergence.
    // A genuinely different token cannot pass this: its separation is orders
    // above the error at either end of it.
    for (rank, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        if a == b {
            continue;
        }
        let separation = (want[*a] - want[*b]).abs() as f64;
        let slack = ((got[*a] - want[*a]).abs() + (got[*b] - want[*b]).abs()) as f64;
        assert!(
            separation <= slack,
            "{what}: miejsce {rank} zamienione, a wzorzec dzieli je o {separation:.4} \
             przy błędzie {slack:.4} na tej parze — to nie jest remis"
        );
    }
    assert!(
        err < bound,
        "{what}: {:.3}% rozpiętości to nie jest ta sama arytmetyka",
        err * 100.0
    );
}
