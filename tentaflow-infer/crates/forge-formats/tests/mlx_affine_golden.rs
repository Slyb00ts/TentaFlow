// ===== File: mlx_affine_golden.rs — MLX affine decode against MLX itself =====
//
// The fixture is produced by tools/mlx-oracle/gen_fixtures.py from a real
// checkpoint using mx.dequantize. It is an oracle, not a second reading of our
// own formula: the bit packing order is the one thing config.json does not
// state, and a wrong guess yields plausible-looking garbage with no error.
//
// Two independent checks per tensor:
//   1. integers alone (scale=1, bias=0) — BIT EXACT, this pins the packing;
//   2. the full affine decode — this pins the direction of the bias, the field
//      where DeepSeek multiplies, compressed-tensors divides and MLX adds.

use half::{bf16, f16};

use forge_formats::{dequantize_affine, MlxAffineTensor, MlxMode, MlxParams, MlxQuantConfig};

/// Two real checkpoints, deliberately different in what they exercise:
/// a dense text model with bf16 scales, and a Whisper encoder-decoder with f16
/// scales, convolution weights left unquantized and the `.bias` / `.biases`
/// name collision.
const FIXTURES: &[(&str, &[u8])] = &[
    ("bielik-7b", include_bytes!("fixtures/mlx_affine_bielik.bin")),
    ("whisper-large-v3-turbo", include_bytes!("fixtures/mlx_affine_whisper.bin")),
];

/// Scales as stored in the file, kept in their own type.
enum Params {
    Bf16(Vec<bf16>),
    F16(Vec<f16>),
}

impl Params {
    fn view(&self) -> MlxParams<'_> {
        match self {
            Params::Bf16(v) => MlxParams::Bf16(v),
            Params::F16(v) => MlxParams::F16(v),
        }
    }

    fn ones(&self) -> Params {
        match self {
            Params::Bf16(v) => Params::Bf16(vec![bf16::ONE; v.len()]),
            Params::F16(v) => Params::F16(vec![f16::ONE; v.len()]),
        }
    }

    fn zeros(&self) -> Params {
        match self {
            Params::Bf16(v) => Params::Bf16(vec![bf16::ZERO; v.len()]),
            Params::F16(v) => Params::F16(vec![f16::ZERO; v.len()]),
        }
    }
}

struct Case {
    name: String,
    rows: usize,
    cols: usize,
    packed: Vec<u32>,
    scales: Params,
    biases: Params,
    expected: Vec<f32>,
    raw_q: Vec<f32>,
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u32(&mut self) -> u32 {
        let v = u32::from_le_bytes(self.buf[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        v
    }

    fn blob(&mut self) -> &'a [u8] {
        let len = self.u32() as usize;
        let out = &self.buf[self.pos..self.pos + len];
        self.pos += len;
        out
    }
}

fn le_u32s(b: &[u8]) -> Vec<u32> {
    b.chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn le_params(b: &[u8], dtype: u32) -> Params {
    let bits: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes(c.try_into().unwrap()))
        .collect();
    match dtype {
        0 => Params::Bf16(bits.into_iter().map(bf16::from_bits).collect()),
        1 => Params::F16(bits.into_iter().map(f16::from_bits).collect()),
        other => panic!("nieznany typ skal w fiksturze: {other}"),
    }
}

fn le_f32s(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
        .collect()
}

fn load(fixture: &'static [u8]) -> (MlxQuantConfig, Vec<Case>) {
    assert_eq!(&fixture[0..4], b"MLXF", "zły magic fikstury");
    let mut r = Reader {
        buf: fixture,
        pos: 4,
    };
    let version = r.u32();
    assert_eq!(version, 2, "nieznana wersja fikstury");
    let group_size = r.u32() as usize;
    let bits = r.u32();
    let count = r.u32() as usize;

    let cfg = MlxQuantConfig {
        group_size,
        bits,
        mode: MlxMode::Affine,
    };

    let mut cases = Vec::with_capacity(count);
    for _ in 0..count {
        let name = String::from_utf8(r.blob().to_vec()).unwrap();
        let rows = r.u32() as usize;
        let _packed_cols = r.u32() as usize;
        let cols = r.u32() as usize;
        let _groups = r.u32() as usize;
        let param_dtype = r.u32();
        cases.push(Case {
            name,
            rows,
            cols,
            packed: le_u32s(r.blob()),
            scales: le_params(r.blob(), param_dtype),
            biases: le_params(r.blob(), param_dtype),
            expected: le_f32s(r.blob()),
            raw_q: le_f32s(r.blob()),
        });
    }
    (cfg, cases)
}

#[test]
fn unpacking_matches_mlx_bit_exact() {
    for (model, fixture) in FIXTURES {
    let (cfg, cases) = load(fixture);
    assert!(!cases.is_empty(), "{model}: fikstura jest pusta");

    for c in &cases {
        let ones = c.scales.ones();
        let zeros = c.biases.zeros();
        let t = MlxAffineTensor {
            packed: &c.packed,
            scales: ones.view(),
            biases: zeros.view(),
            rows: c.rows,
            cols: c.cols,
        };
        let mut out = vec![0f32; c.rows * c.cols];
        dequantize_affine(&t, &cfg, &mut out).unwrap();

        assert_eq!(out.len(), c.raw_q.len(), "{}: rozmiar wyniku", c.name);
        for (i, (got, want)) in out.iter().zip(&c.raw_q).enumerate() {
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "{}: element {i} — rozpakowanie różni się od MLX ({got} wobec {want})",
                c.name
            );
        }

        // Every stored integer must fit the declared bit width; a decoder that
        // shifts by the wrong amount can still land inside the mask, so this is
        // a second, independent way for a packing error to show up.
        let max = ((1u32 << cfg.bits) - 1) as f32;
        assert!(
            out.iter().all(|v| *v >= 0.0 && *v <= max),
            "{}: wartość poza zakresem {} bitów",
            c.name,
            cfg.bits
        );
    }
    }
}

#[test]
fn affine_decode_matches_mlx() {
    for (_model, fixture) in FIXTURES {
    let (cfg, cases) = load(fixture);

    for c in &cases {
        let t = MlxAffineTensor {
            packed: &c.packed,
            scales: c.scales.view(),
            biases: c.biases.view(),
            rows: c.rows,
            cols: c.cols,
        };
        let mut out = vec![0f32; c.rows * c.cols];
        dequantize_affine(&t, &cfg, &mut out).unwrap();

        // The decoder works in f32; MLX returns the checkpoint dtype. Comparing
        // raw f32 bits would measure the width of the accumulator, not the
        // correctness of the formula, so the comparison happens after one
        // rounding to bf16 — and there it must be EXACT. A bias applied with
        // the wrong sign or a scale applied by division cannot survive this.
        let mut mismatches = 0usize;
        let mut worst = 0f32;
        let mut worst_at = 0usize;
        for (i, (got, want)) in out.iter().zip(&c.expected).enumerate() {
            let round = |v: f32| match &c.scales {
                Params::Bf16(_) => bf16::from_f32(v).to_f32(),
                Params::F16(_) => f16::from_f32(v).to_f32(),
            };
            if round(*got).to_bits() != round(*want).to_bits() {
                mismatches += 1;
            }
            let denom = want.abs().max(1e-6);
            let rel = (got - want).abs() / denom;
            if rel > worst {
                worst = rel;
                worst_at = i;
            }
        }

        assert_eq!(
            mismatches, 0,
            "{}: {mismatches} wartości różni się po zaokrągleniu do typu checkpointu; \
             największy błąd względny {worst:.3e} na elemencie {worst_at} \
             (got {}, want {})",
            c.name, out[worst_at], c.expected[worst_at]
        );

        // Second, independent bound: even before rounding the difference must
        // stay inside one bf16 ulp, otherwise the agreement above would be
        // luck rather than arithmetic.
        assert!(
            worst <= 8.0e-3,
            "{}: największy błąd względny {worst:.3e} na elemencie {worst_at}",
            c.name
        );
    }
    }
}
