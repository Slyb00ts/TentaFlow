// ===== File: metal_scatter.rs — rozrzut ogona ANE do wyjścia projekcji, wobec hosta =====
//
// Kernel jest kopią z konwersją, więc jego błędy nie mają kształtu: złe
// przesunięcie daje bufor o poprawnym rozmiarze wypełniony cudzymi liczbami.
// Dlatego porównanie jest element po elemencie, a nie po normie, i sprawdza
// osobno to, co MIAŁO zostać nietknięte: wiersze poza `tokens` i kolumny poza
// oknem `[dst_col0, dst_col0 + count)`.

#![cfg(all(feature = "metal", any(target_os = "macos", target_os = "ios")))]

use forge_hal::metal_device::MetalDevice;
use forge_hal::{Device, LaunchArgs, LaunchConfig, Pool};
use forge_kernels::msl::{self, OutDtype};
use forge_types::MemKind;
use half::f16;

/// Deterministyczny generator: test ma być powtarzalny bez zależności.
struct Lcg(u64);

impl Lcg {
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    /// Wartość f16 z przedziału [-4, 4), dokładnie reprezentowalna.
    fn next_f16(&mut self) -> f16 {
        f16::from_f32((self.next_u32() % 8192) as f32 / 1024.0 - 4.0)
    }
}

/// Jeden przypadek: kształty i okno.
#[derive(Clone, Copy, Debug)]
struct Case {
    t_model: u32,
    tokens: u32,
    src_width: u32,
    src_col0: u32,
    dst_stride: u32,
    dst_col0: u32,
    count: u32,
}

fn f16_bytes(v: &[f16]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_bits().to_le_bytes()).collect()
}

fn f32_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn bytes_f16(b: &[u8]) -> Vec<f16> {
    b.as_chunks::<2>()
        .0
        .iter()
        .map(|c| f16::from_bits(u16::from_le_bytes(*c)))
        .collect()
}

fn bytes_f32(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
        .collect()
}

/// Referencja na hoście: dokładnie kontrakt kernela, nic więcej.
fn reference(c: &Case, src: &[f16], dst: &mut [f32]) {
    for t in 0..c.tokens as usize {
        for k in 0..c.count as usize {
            let s = src[t * c.src_width as usize + c.src_col0 as usize + k];
            dst[t * c.dst_stride as usize + c.dst_col0 as usize + k] = s.to_f32();
        }
    }
}

/// Uruchamia kernel dla obu typów wyjścia i porównuje z referencją.
fn run(dev: &MetalDevice, c: Case, out: OutDtype) {
    let mut rng = Lcg(0x5eed_0000 + u64::from(c.count) * 31 + u64::from(c.tokens));
    let src: Vec<f16> = (0..(c.t_model * c.src_width) as usize)
        .map(|_| rng.next_f16())
        .collect();
    // Wyjście zaczyna od losowego tła, a nie od zer: zero to wartość, którą
    // kernel z błędnym przesunięciem też mógłby „poprawnie" zostawić.
    let dst_len = (c.t_model * c.dst_stride) as usize;
    let background: Vec<f16> = (0..dst_len).map(|_| rng.next_f16()).collect();

    let mut want: Vec<f32> = background.iter().map(|x| x.to_f32()).collect();
    reference(&c, &src, &mut want);

    let elem = match out {
        OutDtype::F16 => 2,
        OutDtype::F32 => 4,
    };
    let src_buf = dev
        .alloc(src.len() * 2, MemKind::Device, Pool::Activations)
        .expect("src");
    let dst_buf = dev
        .alloc(dst_len * elem, MemKind::Device, Pool::Activations)
        .expect("dst");
    dev.write(&f16_bytes(&src), &src_buf, 0).expect("write src");
    let bg_bytes = match out {
        OutDtype::F16 => f16_bytes(&background),
        OutDtype::F32 => f32_bytes(&background.iter().map(|x| x.to_f32()).collect::<Vec<_>>()),
    };
    dev.write(&bg_bytes, &dst_buf, 0).expect("write dst");

    let module = dev
        .load_module(msl::scatter_cols_source(out).as_bytes())
        .expect("kompilacja");
    let kernel = module.kernel(&msl::scatter_cols_name(out)).expect("kernel");
    let stream = dev.create_stream().expect("stream");
    let args = LaunchArgs::new()
        .buf(&src_buf)
        .buf(&dst_buf)
        .scalar(c.src_width)
        .scalar(c.src_col0)
        .scalar(c.dst_stride)
        .scalar(c.dst_col0)
        .scalar(c.count)
        .scalar(c.tokens);
    dev.launch(
        &kernel,
        &LaunchConfig {
            grid: (msl::scatter_groups(c.count, c.tokens), 1, 1),
            block: (msl::SCATTER_THREADS, 1, 1),
            shared_mem_bytes: 0,
        },
        &args,
        &stream,
    )
    .expect("launch");
    stream.synchronize().expect("sync");

    let mut raw = vec![0u8; dst_len * elem];
    dev.read(&dst_buf, 0, &mut raw).expect("read");
    let got: Vec<f32> = match out {
        OutDtype::F16 => bytes_f16(&raw).iter().map(|x| x.to_f32()).collect(),
        OutDtype::F32 => bytes_f32(&raw),
    };

    // Wszystkie wartości są f16, więc konwersja w obie strony jest dokładna i
    // porównanie ma prawo być bitowe.
    let mut inside_touched = 0usize;
    for t in 0..c.t_model as usize {
        for col in 0..c.dst_stride as usize {
            let i = t * c.dst_stride as usize + col;
            let in_window = t < c.tokens as usize
                && col >= c.dst_col0 as usize
                && col < (c.dst_col0 + c.count) as usize;
            assert!(
                got[i].to_bits() == want[i].to_bits(),
                "{c:?} {out:?}: [{t}, {col}] (w oknie: {in_window}) got {} want {}",
                got[i],
                want[i]
            );
            if in_window && got[i].to_bits() != background[i].to_f32().to_bits() {
                inside_touched += 1;
            }
        }
    }
    // Kontrola samego testu: gdyby kernel nic nie zapisał, a referencja też
    // (np. przez zerowy `count`), porównanie przechodziłoby bez znaczenia.
    if c.tokens > 0 && c.count > 0 {
        assert!(
            inside_touched > (c.tokens * c.count) as usize / 2,
            "{c:?}: w oknie zmieniło się tylko {inside_touched} elementów"
        );
    }
}

fn cases() -> Vec<Case> {
    vec![
        // Ogon FFN Bielika przy kaflu równym modelowi: gate/up [T, 11264],
        // ANE oddaje [T, 3072] i pisze w kolumny [8192, 11264).
        Case {
            t_model: 64,
            tokens: 64,
            src_width: 3072,
            src_col0: 0,
            dst_stride: 11264,
            dst_col0: 8192,
            count: 3072,
        },
        // Kafel krótszy niż model: wiersze [tokens, t_model) mają zostać.
        Case {
            t_model: 64,
            tokens: 37,
            src_width: 3072,
            src_col0: 0,
            dst_stride: 11264,
            dst_col0: 8192,
            count: 3072,
        },
        // Źródło szersze niż okno (wyjście ANE wspólne dla kilku projekcji):
        // czytamy środek [1024, 2048) z bufora o szerokości 4096, piszemy w
        // [3072, 4096) wyjścia o kroku 4096.
        Case {
            t_model: 32,
            tokens: 32,
            src_width: 4096,
            src_col0: 1024,
            dst_stride: 4096,
            dst_col0: 3072,
            count: 1024,
        },
        // Najmniejszy ogon: jeden blok.
        Case {
            t_model: 16,
            tokens: 16,
            src_width: 64,
            src_col0: 0,
            dst_stride: 1024,
            dst_col0: 960,
            count: 64,
        },
        // Wywołanie niewyrównane i nie-wielokrotność ośmiu: ścieżka skalarna
        // ma dać ten sam wynik, a nie inny.
        Case {
            t_model: 8,
            tokens: 5,
            src_width: 70,
            src_col0: 3,
            dst_stride: 101,
            dst_col0: 7,
            count: 53,
        },
        // Zero tokenów: nic nie wolno ruszyć.
        Case {
            t_model: 8,
            tokens: 0,
            src_width: 64,
            src_col0: 0,
            dst_stride: 128,
            dst_col0: 64,
            count: 64,
        },
    ]
}

#[test]
fn scatter_moves_exactly_the_window_and_nothing_else() {
    let Ok(dev) = MetalDevice::new() else {
        eprintln!("pomijam: brak urządzenia Metal");
        return;
    };
    for c in cases() {
        run(&dev, c, OutDtype::F16);
        run(&dev, c, OutDtype::F32);
    }
}
