// =============================================================================
// Plik: coreml_backend.rs
// Opis: Sonda wiązania CoreML na prawdziwym modelu .mlmodelc: ładowanie,
//       odczyt kształtu, predict na buforach Metala (Shared) bez kopii,
//       mediana czasu predict. Test jest `#[ignore]` i bez modelu w env
//       FORGE_ANE_PROBE_MODEL kończy się komunikatem "pomijam".
// Przykład:
//       FORGE_ANE_PROBE_MODEL=/sciezka/ffn_T1024_int4.mlmodelc \
//       cargo test -p forge-hal --features metal,coreml --test coreml_backend \
//           -- --ignored --nocapture
// =============================================================================
#![cfg(all(
    feature = "coreml",
    feature = "metal",
    any(target_os = "macos", target_os = "ios")
))]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use forge_hal::coreml::{ComputeUnits, CoreMlModel};
use forge_hal::metal_device::MetalDevice;
use forge_hal::{Device, Pool};
use forge_types::MemKind;

/// Prosty generator xorshift — test nie potrzebuje zależności `rand`, a
/// powtarzalność ziarna ułatwia porównywanie przebiegów.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Losowe f16 z przedziału [-1, 1): losowy znak i mantysa, wykładnik tak
    /// dobrany, żeby wartość mieściła się w [0.5, 1) lub była ćwiartką tego.
    fn f16_unit(&mut self) -> u16 {
        let r = self.next();
        let sign = ((r >> 63) as u16) << 15;
        let exp = 0x3800 - (((r >> 40) & 0x3) as u16) * 0x0400;
        let mant = (r & 0x3FF) as u16;
        sign | exp | mant
    }
}

fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) & 1) as u32;
    let exp = ((h >> 10) & 0x1F) as i32;
    let mant = (h & 0x3FF) as u32;
    let bits = match exp {
        0 => {
            if mant == 0 {
                sign << 31
            } else {
                // Liczby subnormalne: przeskaluj do postaci znormalizowanej.
                let mut e = -1;
                let mut m = mant;
                while m & 0x400 == 0 {
                    m <<= 1;
                    e -= 1;
                }
                (sign << 31) | (((e + 127 - 15 + 1) as u32) << 23) | ((m & 0x3FF) << 13)
            }
        }
        31 => (sign << 31) | 0x7F80_0000 | (mant << 13),
        _ => (sign << 31) | (((exp + 127 - 15) as u32) << 23) | (mant << 13),
    };
    f32::from_bits(bits)
}

#[test]
#[ignore = "wymaga modelu .mlmodelc w FORGE_ANE_PROBE_MODEL"]
fn probe_predict_in_place_on_metal_buffers() {
    let Some(model_path) = std::env::var_os("FORGE_ANE_PROBE_MODEL").map(PathBuf::from) else {
        eprintln!("pomijam: brak FORGE_ANE_PROBE_MODEL");
        return;
    };
    let in_name = std::env::var("FORGE_ANE_PROBE_IN").unwrap_or_else(|_| "x".into());
    let out_name = std::env::var("FORGE_ANE_PROBE_OUT").unwrap_or_else(|_| "y".into());

    let dev = match MetalDevice::new() {
        Ok(dev) => dev,
        Err(e) => {
            eprintln!("pomijam: brak urządzenia Metal: {e}");
            return;
        }
    };

    let t_load = Instant::now();
    let model = CoreMlModel::load(&model_path, None, ComputeUnits::CpuAndNeuralEngine)
        .expect("ładowanie modelu CoreML");
    eprintln!(
        "model {} załadowany w {:?}",
        model_path.display(),
        t_load.elapsed()
    );

    let shape = model.shape(&in_name, &out_name).expect("kształt modelu");
    eprintln!(
        "kształt: {in_name} [{}, {}] -> {out_name} [{}, {}]",
        shape.in_rows, shape.in_cols, shape.out_rows, shape.out_cols
    );
    assert!(shape.in_rows > 0 && shape.in_cols > 0);
    assert!(shape.out_rows > 0 && shape.out_cols > 0);

    // Bufory Shared z Metala: wskaźnik hosta wyrównany do strony, więc CoreML
    // może podstawić je pod MLMultiArray bez własnej kopii.
    let x_buf = dev
        .alloc(shape.in_bytes(), MemKind::Device, Pool::Activations)
        .expect("alokacja x");
    let y_buf = dev
        .alloc(shape.out_bytes(), MemKind::Device, Pool::Activations)
        .expect("alokacja y");
    let x_ptr = x_buf.host_ptr().expect("Metal daje wskaźnik hosta");
    let y_ptr = y_buf.host_ptr().expect("Metal daje wskaźnik hosta");

    let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
    let x_elems = shape.in_rows as usize * shape.in_cols as usize;
    // SAFETY: bufor ma dokładnie x_elems * 2 bajtów i nikt inny go nie dotyka.
    let x_slice = unsafe { std::slice::from_raw_parts_mut(x_ptr as *mut u16, x_elems) };
    for v in x_slice.iter_mut() {
        *v = rng.f16_unit();
    }
    let y_elems = shape.out_rows as usize * shape.out_cols as usize;
    // SAFETY: jak wyżej, bufor wyjściowy.
    let y_slice = unsafe { std::slice::from_raw_parts_mut(y_ptr as *mut u16, y_elems) };
    y_slice.fill(0xFFFF);

    // SAFETY: x i y to osobne, żywe bufory o rozmiarach z opisu modelu.
    let in_place = unsafe { model.predict(&in_name, x_ptr, &out_name, y_ptr) }.expect("predict");
    eprintln!("pierwszy predict: in-place = {in_place}");
    assert!(
        in_place,
        "CoreML nie uszanował outputBackings — wynik został skopiowany"
    );

    // Wynik ma być skończony i nie może pozostać wypełnieniem 0xFFFF (NaN).
    let finite = y_slice
        .iter()
        .filter(|h| f16_to_f32(**h).is_finite())
        .count();
    assert_eq!(
        finite,
        y_elems,
        "{} z {} elementów wyjścia nie jest skończonych",
        y_elems - finite,
        y_elems
    );

    // Rozgrzewka, potem mediana z 20 przebiegów.
    for _ in 0..5 {
        // SAFETY: jak wyżej.
        unsafe { model.predict(&in_name, x_ptr, &out_name, y_ptr) }.expect("predict rozgrzewka");
    }
    let mut times: Vec<Duration> = Vec::with_capacity(20);
    let mut all_in_place = true;
    for _ in 0..20 {
        let t = Instant::now();
        // SAFETY: jak wyżej.
        let ok = unsafe { model.predict(&in_name, x_ptr, &out_name, y_ptr) }.expect("predict");
        times.push(t.elapsed());
        all_in_place &= ok;
    }
    times.sort();
    let median = times[times.len() / 2];
    let min = times[0];
    let max = times[times.len() - 1];
    eprintln!(
        "predict {}x{} -> {}x{}: mediana {:.3} ms (min {:.3}, max {:.3}), in-place: {all_in_place}",
        shape.in_rows,
        shape.in_cols,
        shape.out_rows,
        shape.out_cols,
        median.as_secs_f64() * 1e3,
        min.as_secs_f64() * 1e3,
        max.as_secs_f64() * 1e3
    );
    assert!(all_in_place);

    // Wariant ze skokiem wiersza: wynik w kolumnowym wycinku dwukrotnie
    // szerszej macierzy. Sprawdza, że strides trafiają do CoreML i że wynik
    // zgadza się z ciągłym.
    let stride = shape.out_cols * 2;
    let wide_bytes = shape.out_rows as usize * stride as usize * 2;
    let wide_buf = dev
        .alloc(wide_bytes, MemKind::Device, Pool::Activations)
        .expect("alokacja wide");
    let wide_ptr = wide_buf.host_ptr().expect("wskaźnik hosta");
    // SAFETY: bufor wide ma wide_bytes bajtów, nie nakłada się z x.
    let wide_slice =
        unsafe { std::slice::from_raw_parts_mut(wide_ptr as *mut u16, wide_bytes / 2) };
    wide_slice.fill(0x1234);
    // SAFETY: x żywy, wide obejmuje (rows-1)*stride+cols elementów.
    let strided_in_place =
        unsafe { model.predict_strided(&in_name, x_ptr, &out_name, wide_ptr, stride) }
            .expect("predict_strided");
    eprintln!("predict_strided (stride {stride}): in-place = {strided_in_place}");
    for r in 0..shape.out_rows as usize {
        let row = &wide_slice[r * stride as usize..r * stride as usize + shape.out_cols as usize];
        let reference = &y_slice[r * shape.out_cols as usize..(r + 1) * shape.out_cols as usize];
        assert_eq!(row, reference, "wiersz {r} różni się od wyniku ciągłego");
        let pad =
            &wide_slice[r * stride as usize + shape.out_cols as usize..(r + 1) * stride as usize];
        assert!(
            pad.iter().all(|v| *v == 0x1234),
            "wiersz {r}: wypełnienie poza kolumnami zostało nadpisane"
        );
    }
}
