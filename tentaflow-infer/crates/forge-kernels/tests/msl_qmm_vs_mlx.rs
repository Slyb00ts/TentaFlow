// ===== File: msl_qmm_vs_mlx.rs — the batched dequant-matmul, the prefill lever =====
//
// Same operation as the GEMV next door, run on many tokens at once. This is
// where prefill time actually goes: a decode step reads the entire weight
// matrix to produce one token, and the only way to stop paying that per token
// is to let one pass over the weights serve a whole tile of them.
//
// Two gates, and they check different things.
//
// Against the MLX oracle and an f64 truth: the arithmetic is right, per token
// row, including the tail of a tile that is not full.
//
// Against the vector form, BIT FOR BIT: prefill and decode must agree exactly
// on the same input. They accumulate in the same order by construction, so any
// difference here is a layout or indexing fault, not rounding — and it is worth
// a separate test because a kernel can be numerically plausible and still write
// token 3's answer into token 4's row.
//
// Fixture: tools/mlx-oracle/gen_qmm.py
#![cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]

use std::sync::Arc;

use forge_hal::metal_device::MetalDevice;
use forge_hal::{Device, DevBuffer, LaunchArgs, LaunchConfig, Pool};
use forge_kernels::msl::{self, OutDtype, ScaleDtype};
use forge_types::MemKind;

const FIXTURE: &[u8] = include_bytes!("fixtures/mlx_qmm_bielik.bin");

struct Case {
    name: String,
    rows: u32,
    cols: u32,
    packed: Vec<u8>,
    scales: Vec<u8>,
    biases: Vec<u8>,
    /// Aktywacje `[tokens, cols]` w f16, wiersz po wierszu.
    x: Vec<u8>,
    /// Wynik ścieżki MLX `[tokens, rows]` w f32.
    y_mlx: Vec<f32>,
    /// Prawda w f64, ten sam kształt.
    y_true: Vec<f64>,
}

fn load() -> (u32, u32, u32, Vec<Case>) {
    assert_eq!(&FIXTURE[0..4], b"QMM1", "zły magic fikstury");
    let mut pos = 4usize;
    fn u32_at(pos: &mut usize) -> u32 {
        let v = u32::from_le_bytes(FIXTURE[*pos..*pos + 4].try_into().unwrap());
        *pos += 4;
        v
    }
    fn blob(pos: &mut usize) -> Vec<u8> {
        let len = u32_at(pos) as usize;
        let out = FIXTURE[*pos..*pos + len].to_vec();
        *pos += len;
        out
    }
    assert_eq!(u32_at(&mut pos), 1, "wersja fikstury");
    let group = u32_at(&mut pos);
    let bits = u32_at(&mut pos);
    let tokens = u32_at(&mut pos);
    let count = u32_at(&mut pos);

    let mut cases = Vec::new();
    for _ in 0..count {
        let name_len = u32_at(&mut pos) as usize;
        let name = String::from_utf8(FIXTURE[pos..pos + name_len].to_vec()).unwrap();
        pos += name_len;
        let rows = u32_at(&mut pos);
        let cols = u32_at(&mut pos);
        let (packed, scales, biases, x, y, y64) = (
            blob(&mut pos),
            blob(&mut pos),
            blob(&mut pos),
            blob(&mut pos),
            blob(&mut pos),
            blob(&mut pos),
        );
        cases.push(Case {
            name,
            rows,
            cols,
            packed,
            scales,
            biases,
            x,
            y_mlx: y
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect(),
            y_true: y64
                .chunks_exact(8)
                .map(|c| f64::from_le_bytes(c.try_into().unwrap()))
                .collect(),
        });
    }
    (group, bits, tokens, cases)
}

fn rel_l2_f64(got: &[f64], want: &[f64]) -> f64 {
    let num: f64 = got.iter().zip(want).map(|(g, w)| (g - w).powi(2)).sum();
    let den: f64 = want.iter().map(|w| w * w).sum();
    (num / den.max(f64::MIN_POSITIVE)).sqrt()
}

struct Gpu {
    dev: Arc<MetalDevice>,
    stream: forge_hal::Stream,
}

impl Gpu {
    fn upload(&self, bytes: &[u8]) -> DevBuffer {
        let buf = self
            .dev
            .alloc(bytes.len(), MemKind::Device, Pool::Weights)
            .unwrap();
        self.dev.write(bytes, &buf, 0).unwrap();
        buf
    }

    fn read_f32(&self, buf: &DevBuffer, len: usize) -> Vec<f32> {
        let mut bytes = vec![0u8; len * 4];
        self.dev.read(buf, 0, &mut bytes).unwrap();
        bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect()
    }
}

fn gpu() -> Option<Gpu> {
    let dev = MetalDevice::new().ok()?;
    let stream = dev.create_stream().unwrap();
    Some(Gpu { dev, stream })
}

#[test]
fn batched_matmul_is_no_further_from_the_truth_than_mlx() {
    let (group, bits, tokens, cases) = load();
    assert_eq!(bits, 4, "ten kernel obsługuje wyłącznie 4 bity");
    assert_ne!(
        tokens % msl::QMM_TILE,
        0,
        "fikstura ma pełne kafle, więc ogon kernela nie jest sprawdzany"
    );
    let Some(g) = gpu() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };

    let (sd, od) = (ScaleDtype::Bf16, OutDtype::F32);
    let module = g
        .dev
        .load_module(msl::qmm_affine_source(msl::Bits::Four, sd, od).as_bytes())
        .unwrap();
    let kernel = module.kernel(&msl::qmm_affine_name(msl::Bits::Four, sd, od)).unwrap();

    for c in &cases {
        let packed = g.upload(&c.packed);
        let scales = g.upload(&c.scales);
        let biases = g.upload(&c.biases);
        let x = g.upload(&c.x);
        let out_len = (tokens * c.rows) as usize;
        let y = g
            .dev
            .alloc(out_len * 4, MemKind::Device, Pool::Activations)
            .unwrap();

        let (gx, gy) = msl::qmm_affine_4bit_groups(c.rows, tokens);
        g.dev
            .launch(
                &kernel,
                &LaunchConfig {
                    grid: (gx, gy, 1),
                    block: (msl::QMM_THREADS, 1, 1),
                    shared_mem_bytes: 0,
                },
                &LaunchArgs::new()
                    .buf(&y)
                    .buf(&packed)
                    .buf(&scales)
                    .buf(&biases)
                    .buf(&x)
                    .scalar(c.rows)
                    .scalar(c.cols)
                    .scalar(group)
                    .scalar(tokens),
                &g.stream,
            )
            .unwrap();
        g.stream.synchronize().unwrap();
        let got = g.read_f32(&y, out_len);

        // Per token, nie na całej macierzy naraz: błąd w JEDNYM wierszu ginie
        // w normie liczonej po wszystkich trzynastu, a to właśnie pojedynczy
        // wiersz — ostatni, niepełny — jest tutaj najbardziej podejrzany.
        for t in 0..tokens as usize {
            let lo = t * c.rows as usize;
            let hi = lo + c.rows as usize;
            let ours: Vec<f64> = got[lo..hi].iter().map(|v| *v as f64).collect();
            let mlx: Vec<f64> = c.y_mlx[lo..hi].iter().map(|v| *v as f64).collect();
            let truth = &c.y_true[lo..hi];

            let mlx_err = rel_l2_f64(&mlx, truth);
            let our_err = rel_l2_f64(&ours, truth);
            assert!(
                our_err <= mlx_err,
                "{} token {t}: kernel odbiega od prawdy o {our_err:.3e}, MLX o {mlx_err:.3e}",
                c.name
            );
            assert!(
                our_err < 1.0e-5,
                "{} token {t}: kernel odbiega od prawdy o {our_err:.3e}",
                c.name
            );
        }
        eprintln!("{}: {tokens} tokenów, {} wierszy — zgodne", c.name, c.rows);
    }
}

#[test]
fn the_matrix_unit_form_is_no_further_from_the_truth_than_mlx() {
    // Ta forma NIE jest bitowo zgodna z wektorową — jednostka macierzowa sumuje
    // po swojemu — więc trzyma ją ten sam próg co MLX: nie dalej od prawdy w f64
    // niż sama wyrocznia. Wagi idą do niej przez half, czyli 11 bitów mantysy,
    // a MLX dekwantyzuje do bf16, czyli ośmiu; gdyby ta droga była gorsza, ten
    // próg by to pokazał.
    let (group, _bits, tokens, cases) = load();
    let Some(g) = gpu() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };

    let (sd, od) = (ScaleDtype::Bf16, OutDtype::F32);
    let module = g
        .dev
        .load_module(msl::qmg_affine_source(msl::Bits::Four, sd, od).as_bytes())
        .unwrap();
    let kernel = module.kernel(&msl::qmg_affine_name(msl::Bits::Four, sd, od)).unwrap();

    for c in &cases {
        assert!(
            msl::qmg_fits(c.rows, c.cols),
            "{}: kształt [{}, {}] nie pasuje do bloku",
            c.name,
            c.rows,
            c.cols
        );
        let packed = g.upload(&c.packed);
        let scales = g.upload(&c.scales);
        let biases = g.upload(&c.biases);
        let x = g.upload(&c.x);
        let y = g
            .dev
            .alloc((tokens * c.rows) as usize * 4, MemKind::Device, Pool::Activations)
            .unwrap();

        let (gx, gy) = msl::qmg_affine_4bit_groups(c.rows, tokens);
        g.dev
            .launch(
                &kernel,
                &LaunchConfig {
                    grid: (gx, gy, 1),
                    block: (msl::QMG_THREADS, 1, 1),
                    shared_mem_bytes: 0,
                },
                &LaunchArgs::new()
                    .buf(&y)
                    .buf(&packed)
                    .buf(&scales)
                    .buf(&biases)
                    .buf(&x)
                    .scalar(c.rows)
                    .scalar(c.cols)
                    .scalar(group)
                    .scalar(tokens),
                &g.stream,
            )
            .unwrap();
        g.stream.synchronize().unwrap();
        let got = g.read_f32(&y, (tokens * c.rows) as usize);

        for t in 0..tokens as usize {
            let lo = t * c.rows as usize;
            let hi = lo + c.rows as usize;
            let ours: Vec<f64> = got[lo..hi].iter().map(|v| *v as f64).collect();
            let mlx: Vec<f64> = c.y_mlx[lo..hi].iter().map(|v| *v as f64).collect();
            let truth = &c.y_true[lo..hi];
            let mlx_err = rel_l2_f64(&mlx, truth);
            let our_err = rel_l2_f64(&ours, truth);
            assert!(
                our_err <= mlx_err,
                "{} token {t}: jednostka macierzowa odbiega od prawdy o {our_err:.3e}, \
                 MLX o {mlx_err:.3e}",
                c.name
            );
        }
        eprintln!("{}: jednostka macierzowa zgodna na {tokens} tokenach", c.name);
    }
}

#[test]
fn the_batched_form_agrees_with_the_vector_form_bit_for_bit() {
    let (group, _bits, tokens, cases) = load();
    let Some(g) = gpu() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };

    let (sd, od) = (ScaleDtype::Bf16, OutDtype::F32);
    let mm = g
        .dev
        .load_module(msl::qmm_affine_source(msl::Bits::Four, sd, od).as_bytes())
        .unwrap();
    let mm_kernel = mm.kernel(&msl::qmm_affine_name(msl::Bits::Four, sd, od)).unwrap();
    let mv = g
        .dev
        .load_module(msl::qmv_affine_source(msl::Bits::Four, sd, od).as_bytes())
        .unwrap();
    let mv_kernel = mv.kernel(&msl::qmv_affine_name(msl::Bits::Four, sd, od)).unwrap();

    let c = &cases[0];
    let packed = g.upload(&c.packed);
    let scales = g.upload(&c.scales);
    let biases = g.upload(&c.biases);
    let x = g.upload(&c.x);
    let out_len = (tokens * c.rows) as usize;
    let y_mm = g
        .dev
        .alloc(out_len * 4, MemKind::Device, Pool::Activations)
        .unwrap();

    let (gx, gy) = msl::qmm_affine_4bit_groups(c.rows, tokens);
    g.dev
        .launch(
            &mm_kernel,
            &LaunchConfig {
                grid: (gx, gy, 1),
                block: (msl::QMM_THREADS, 1, 1),
                shared_mem_bytes: 0,
            },
            &LaunchArgs::new()
                .buf(&y_mm)
                .buf(&packed)
                .buf(&scales)
                .buf(&biases)
                .buf(&x)
                .scalar(c.rows)
                .scalar(c.cols)
                .scalar(group)
                .scalar(tokens),
            &g.stream,
        )
        .unwrap();
    g.stream.synchronize().unwrap();
    let batched = g.read_f32(&y_mm, out_len);

    let row_bytes = c.cols as usize * 2;
    let y_mv = g
        .dev
        .alloc(c.rows as usize * 4, MemKind::Device, Pool::Activations)
        .unwrap();
    let mut distinct = 0usize;
    for t in 0..tokens as usize {
        let one = g.upload(&c.x[t * row_bytes..(t + 1) * row_bytes]);
        g.dev
            .launch(
                &mv_kernel,
                &LaunchConfig {
                    grid: (msl::qmv_affine_4bit_groups(c.rows), 1, 1),
                    block: (msl::QMV_THREADS, 1, 1),
                    shared_mem_bytes: 0,
                },
                &LaunchArgs::new()
                    .buf(&y_mv)
                    .buf(&packed)
                    .buf(&scales)
                    .buf(&biases)
                    .buf(&one)
                    .scalar(c.rows)
                    .scalar(c.cols)
                    .scalar(group),
                &g.stream,
            )
            .unwrap();
        g.stream.synchronize().unwrap();
        let single = g.read_f32(&y_mv, c.rows as usize);
        let lo = t * c.rows as usize;
        assert_eq!(
            &batched[lo..lo + c.rows as usize],
            &single[..],
            "token {t}: forma batchowa i wektorowa liczą inaczej"
        );
        if t > 0 && single != batched[0..c.rows as usize] {
            distinct += 1;
        }
    }
    // Bez tego zgodność powyżej byłaby też zgodnością kernela, który każdemu
    // tokenowi zwraca wynik tokenu zerowego.
    assert_eq!(
        distinct,
        tokens as usize - 1,
        "tokeny w fiksturze nie są od siebie różne, więc test niczego nie dzieli"
    );
}

/// Pomiar dźwigni, uruchamiany jawnie: `cargo test ... -- --ignored --nocapture`.
///
/// Nie jest bramką, bo czas na współdzielonej maszynie nie jest wielkością
/// powtarzalną. Jest odpowiedzią na pytanie, czy kafel w ogóle robi to, po co
/// powstał — a to pytanie trzeba zadać PRZED wpięciem go w model.
#[test]
#[ignore]
fn how_much_the_tile_actually_buys() {
    // Sam rozmiar grupy kwantyzacji, bo wagę bierzemy syntetyczną w pełnym
    // rozmiarze warstwy — ale rozmiar grupy ma zostać ten, co w checkpoincie.
    let group = load().0;
    let Some(g) = gpu() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    let (sd, od) = (ScaleDtype::Bf16, OutDtype::F32);
    let mm = g
        .dev
        .load_module(msl::qmm_affine_source(msl::Bits::Four, sd, od).as_bytes())
        .unwrap();
    let mm_kernel = mm.kernel(&msl::qmm_affine_name(msl::Bits::Four, sd, od)).unwrap();
    let mv = g
        .dev
        .load_module(msl::qmv_affine_source(msl::Bits::Four, sd, od).as_bytes())
        .unwrap();
    let mv_kernel = mv.kernel(&msl::qmv_affine_name(msl::Bits::Four, sd, od)).unwrap();
    let mg = g
        .dev
        .load_module(msl::qmg_affine_source(msl::Bits::Four, sd, od).as_bytes())
        .unwrap();
    let mg_kernel = mg.kernel(&msl::qmg_affine_name(msl::Bits::Four, sd, od)).unwrap();

    // Pełny kształt warstwy, nie wycinek z fikstury. To jest cała różnica:
    // 128 wierszy Bielika to 720 KB wagi, która mieści się w cache, więc pętla
    // GEMV nie płaci tam za nic, po co kafel powstał. Prawdziwe `down_proj` ma
    // 4096 wierszy i 23 MB — dopiero to jest ruch, który da się zaoszczędzić.
    let (rows, cols) = (4096u32, 11264u32);
    let groups_per_row = (cols / group) as usize;
    let packed = g.upload(&vec![0x51u8; rows as usize * cols as usize / 2]);
    let scales = g.upload(&vec![0x38u8; rows as usize * groups_per_row * 2]);
    let biases = g.upload(&vec![0x00u8; rows as usize * groups_per_row * 2]);

    for &tokens in &[1u32, 8, 32, 64, 128, 192, 256] {
        let row = cols as usize * 2;
        let x = g.upload(&vec![0x3Cu8; tokens as usize * row]);
        let y = g
            .dev
            .alloc((tokens * rows) as usize * 4, MemKind::Device, Pool::Activations)
            .unwrap();

        let (gx, gy) = msl::qmm_affine_4bit_groups(rows, tokens);
        let cfg_mm = LaunchConfig {
            grid: (gx, gy, 1),
            block: (msl::QMM_THREADS, 1, 1),
            shared_mem_bytes: 0,
        };
        let args_mm = LaunchArgs::new()
            .buf(&y)
            .buf(&packed)
            .buf(&scales)
            .buf(&biases)
            .buf(&x)
            .scalar(rows)
            .scalar(cols)
            .scalar(group)
            .scalar(tokens);

        const REPS: u32 = 10;
        g.dev.launch(&mm_kernel, &cfg_mm, &args_mm, &g.stream).unwrap();
        g.stream.synchronize().unwrap();
        let t0 = std::time::Instant::now();
        for _ in 0..REPS {
            g.dev.launch(&mm_kernel, &cfg_mm, &args_mm, &g.stream).unwrap();
        }
        g.stream.synchronize().unwrap();
        let batched = t0.elapsed().as_secs_f64() / REPS as f64;

        let cfg_mv = LaunchConfig {
            grid: (msl::qmv_affine_4bit_groups(rows), 1, 1),
            block: (msl::QMV_THREADS, 1, 1),
            shared_mem_bytes: 0,
        };
        let t0 = std::time::Instant::now();
        for _ in 0..REPS {
            for t in 0..tokens {
                let args = LaunchArgs::new()
                    .buf(&y)
                    .buf(&packed)
                    .buf(&scales)
                    .buf(&biases)
                    .buf_at(&x, t as usize * row).unwrap()
                    .scalar(rows)
                    .scalar(cols)
                    .scalar(group);
                g.dev.launch(&mv_kernel, &cfg_mv, &args, &g.stream).unwrap();
            }
        }
        g.stream.synchronize().unwrap();
        let looped = t0.elapsed().as_secs_f64() / REPS as f64;

        let (ggx, ggy) = msl::qmg_affine_4bit_groups(rows, tokens);
        let cfg_mg = LaunchConfig {
            grid: (ggx, ggy, 1),
            block: (msl::QMG_THREADS, 1, 1),
            shared_mem_bytes: 0,
        };
        let args_mg = LaunchArgs::new()
            .buf(&y)
            .buf(&packed)
            .buf(&scales)
            .buf(&biases)
            .buf(&x)
            .scalar(rows)
            .scalar(cols)
            .scalar(group)
            .scalar(tokens);
        g.dev.launch(&mg_kernel, &cfg_mg, &args_mg, &g.stream).unwrap();
        g.stream.synchronize().unwrap();
        let t0 = std::time::Instant::now();
        for _ in 0..REPS {
            g.dev.launch(&mg_kernel, &cfg_mg, &args_mg, &g.stream).unwrap();
        }
        g.stream.synchronize().unwrap();
        let matrix = t0.elapsed().as_secs_f64() / REPS as f64;

        eprintln!(
            "T={tokens:4}: macierzowo {:8.1} us ({:6.1} us/token), kafel {:8.1} us \
             ({:6.1} us/token), pętla GEMV {:9.1} us, przyspieszenie {:5.2}x",
            matrix * 1e6,
            matrix * 1e6 / tokens as f64,
            batched * 1e6,
            batched * 1e6 / tokens as f64,
            looped * 1e6,
            looped / matrix
        );
    }
}

/// Obie formy prefillowe na sześciu bitach, przypięte do formy wektorowej.
///
/// Wektorowa jest już przypięta do wzorca CPU, więc porównanie z nią sprawdza
/// dokładnie to, co może się rozjechać: czy blokowa i macierzowa wyłuskują kod
/// tak samo, mimo że robią to w innym miejscu pętli.
#[test]
fn the_six_bit_prefill_forms_agree_with_the_vector_form() {
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    const ROWS: usize = 128;
    const COLS: usize = 256;
    const GROUP: usize = 16;
    const TOKENS: usize = 64;

    let codes: Vec<u8> = (0..ROWS * COLS).map(|i| ((i * 13 + 5) % 64) as u8).collect();
    let groups = ROWS * COLS / GROUP;
    let scales: Vec<half::f16> = (0..groups)
        .map(|i| half::f16::from_f32(0.0015 + (i % 7) as f32 * 0.0002))
        .collect();
    let biases: Vec<half::f16> = (0..groups)
        .map(|i| half::f16::from_f32(-0.02 - (i % 4) as f32 * 0.0008))
        .collect();
    let x: Vec<half::f16> = (0..TOKENS * COLS)
        .map(|i| half::f16::from_f32((i % 13) as f32 * 0.04 - 0.24))
        .collect();

    let mut packed = vec![0u32; ROWS * COLS / 8];
    let mut high = vec![0u32; ROWS * COLS / 16];
    for (i, &c) in codes.iter().enumerate() {
        packed[i / 8] |= u32::from(c & 0xF) << ((i % 8) * 4);
        high[i / 16] |= u32::from((c >> 4) & 0x3) << ((i % 16) * 2);
    }

    let stream = dev.create_stream().unwrap();
    let up = |b: &[u8]| {
        let buf = dev.alloc(b.len(), MemKind::Device, Pool::Weights).unwrap();
        dev.write(b, &buf, 0).unwrap();
        buf
    };
    let (bp, bs, bb, bx, bh) = (
        up(bytemuck::cast_slice(&packed)),
        up(bytemuck::cast_slice(&scales)),
        up(bytemuck::cast_slice(&biases)),
        up(bytemuck::cast_slice(&x)),
        up(bytemuck::cast_slice(&high)),
    );

    // Wzorzec: forma wektorowa, token po tokenie.
    let vec_src = msl::qmv_affine_source(msl::Bits::Six, ScaleDtype::F16, OutDtype::F32);
    let vec_mod = dev.load_module(vec_src.as_bytes()).unwrap();
    let vec_k = vec_mod
        .kernel(&msl::qmv_affine_name(msl::Bits::Six, ScaleDtype::F16, OutDtype::F32))
        .unwrap();
    let mut want = vec![0f32; TOKENS * ROWS];
    for t in 0..TOKENS {
        let row = dev.alloc(ROWS * 4, MemKind::Device, Pool::Activations).unwrap();
        let xt = up(bytemuck::cast_slice(&x[t * COLS..(t + 1) * COLS]));
        dev.launch(
            &vec_k,
            &LaunchConfig {
                grid: (msl::qmv_affine_4bit_groups(ROWS as u32), 1, 1),
                block: (msl::QMV_THREADS, 1, 1),
                shared_mem_bytes: 0,
            },
            &LaunchArgs::new()
                .buf(&row).buf(&bp).buf(&bs).buf(&bb).buf(&xt).buf(&bh)
                .scalar(ROWS as u32).scalar(COLS as u32).scalar(GROUP as u32),
            &stream,
        )
        .unwrap();
        stream.synchronize().unwrap();
        let mut raw = vec![0u8; ROWS * 4];
        dev.read(&row, 0, &mut raw).unwrap();
        for (r, c) in raw.chunks_exact(4).enumerate() {
            want[t * ROWS + r] = f32::from_le_bytes(c.try_into().unwrap());
        }
    }

    for (label, src, name, grid, threads) in [
        (
            "blokowa",
            msl::qmm_affine_source(msl::Bits::Six, ScaleDtype::F16, OutDtype::F32),
            msl::qmm_affine_name(msl::Bits::Six, ScaleDtype::F16, OutDtype::F32),
            msl::qmm_affine_4bit_groups(ROWS as u32, TOKENS as u32),
            msl::QMM_THREADS,
        ),
        (
            "macierzowa",
            msl::qmg_affine_source(msl::Bits::Six, ScaleDtype::F16, OutDtype::F32),
            msl::qmg_affine_name(msl::Bits::Six, ScaleDtype::F16, OutDtype::F32),
            msl::qmg_affine_4bit_groups(ROWS as u32, TOKENS as u32),
            msl::QMG_THREADS,
        ),
    ] {
        let module = dev.load_module(src.as_bytes()).unwrap();
        let kernel = module.kernel(&name).unwrap();
        let out = dev
            .alloc(TOKENS * ROWS * 4, MemKind::Device, Pool::Activations)
            .unwrap();
        dev.launch(
            &kernel,
            &LaunchConfig {
                grid: (grid.0, grid.1, 1),
                block: (threads, 1, 1),
                shared_mem_bytes: 0,
            },
            &LaunchArgs::new()
                .buf(&out).buf(&bp).buf(&bs).buf(&bb).buf(&bx).buf(&bh)
                .scalar(ROWS as u32).scalar(COLS as u32).scalar(GROUP as u32)
                .scalar(TOKENS as u32),
            &stream,
        )
        .unwrap();
        stream.synchronize().unwrap();
        let mut raw = vec![0u8; TOKENS * ROWS * 4];
        dev.read(&out, 0, &mut raw).unwrap();

        let span = want.iter().fold(0f32, |m, v| m.max(v.abs()));
        let mut worst = 0f32;
        for (i, c) in raw.chunks_exact(4).enumerate() {
            worst = worst.max((f32::from_le_bytes(c.try_into().unwrap()) - want[i]).abs());
        }
        assert!(
            worst <= span * 2e-3,
            "forma {label} na sześciu bitach rozjeżdża się z wektorową: {worst:.3e} przy {span:.3e}"
        );
    }
}
