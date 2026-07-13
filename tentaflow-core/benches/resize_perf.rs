// =============================================================================
// Plik: benches/resize_perf.rs
// Opis: Benchmark perf resizera RGB24 (vision::resize) na realnych zdjeciach
//       ADR PoC vs image::imageops::resize (Triangle). Mierzy downscale do
//       560x560 i 1280x720 — czas/obraz i przepustowosc MPx/s.
// =============================================================================

use std::hint::black_box;
use std::path::Path;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use image::{imageops::FilterType, RgbImage};

use tentaflow_core::vision::resize::resize_rgb;

/// Katalog z realnymi zdjeciami ADR (PoC AI). Jesli niedostepny (CI bez
/// montowanego dysku), bench loguje i konczy bez panic.
const ADR_DIR: &str = "/mnt/abyss/Files/Dokumenty/Praca/Euvic/Tematy/ADR/PoC AI/Zdjęcia/5";

/// Zdjecia do benchu — kilka klatek ~5152x3864.
const SAMPLES: &[&str] = &["DSCN9751.JPG", "DSCN9752.JPG", "DSCN9753.JPG"];

/// Wczytuje i dekoduje zdjecie do (RGB24 bytes, w, h).
fn load_rgb(path: &Path) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::open(path).ok()?.to_rgb8();
    let (w, h) = img.dimensions();
    Some((img.into_raw(), w, h))
}

fn bench_resize(c: &mut Criterion) {
    let dir = Path::new(ADR_DIR);
    let mut images: Vec<(String, Vec<u8>, u32, u32)> = Vec::new();
    for name in SAMPLES {
        let p = dir.join(name);
        if let Some((buf, w, h)) = load_rgb(&p) {
            images.push((name.to_string(), buf, w, h));
        }
    }

    if images.is_empty() {
        eprintln!("resize_perf: brak zdjec ADR w {ADR_DIR} — bench pominiety (zamontuj dysk)");
        return;
    }

    // Wymiary docelowe wg naszego use-case'u.
    let targets: &[(u32, u32, &str)] = &[(560, 560, "560x560"), (1280, 720, "1280x720")];

    for (name, src, sw, sh) in &images {
        let src_mpx = (*sw as f64 * *sh as f64) / 1.0e6;

        for &(dw, dh, label) in targets {
            let mut group = c.benchmark_group(format!("{name}/{label}"));
            group.measurement_time(Duration::from_secs(8));
            // Throughput liczony w pikselach wejscia (downscale = praca ~ src).
            group.throughput(Throughput::Elements((*sw as u64) * (*sh as u64)));

            group.bench_function("tentaflow_simd", |b| {
                b.iter(|| {
                    let out = resize_rgb(black_box(src), *sw, *sh, dw, dh).unwrap();
                    black_box(out);
                })
            });

            group.bench_function("image_triangle", |b| {
                let img = RgbImage::from_raw(*sw, *sh, src.clone()).unwrap();
                b.iter(|| {
                    let out =
                        image::imageops::resize(black_box(&img), dw, dh, FilterType::Triangle);
                    black_box(out);
                })
            });

            group.finish();
            let _ = src_mpx;
        }
    }
}

criterion_group!(benches, bench_resize);
criterion_main!(benches);
