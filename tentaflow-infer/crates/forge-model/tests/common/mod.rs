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
