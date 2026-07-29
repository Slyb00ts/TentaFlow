// Sprawdza podzial kolumnowy z REDUKCJA na dwoch kartach: projekcja `down`
// z FFN policzona po kawalku na kazdej karcie i zsumowana, porownana z tym
// samym mnozeniem wykonanym w calosci na jednej karcie.
//
// To jest brakujacy klocek tensor parallel dla FFN. `gate`/`up` dziela sie po
// wierszach (kazda karta dostaje swoj kawalek wymiaru posredniego), a `down`
// po kolumnach zjada dokladnie ten kawalek — dzieki temu na cala warstwe FFN
// przypada JEDNA wymiana, a nie dwie.
use forge_engine::cluster::Cluster;
use forge_engine::multi_gpu::{DeviceCapability, WorkKind};
use forge_engine::tensor_parallel::{BlockFormat, gemv_q8_0_column_split, upload_column_split};
use forge_hal::{Pool, PoolSizes};
use forge_types::{MemKind, QuantKind};
use std::time::Instant;

/// Zapis f32 jako f16 bez zewnetrznej zaleznosci — probe ma byc samowystarczalny.
fn f16_bytes(value: f32) -> [u8; 2] {
    let bits = value.to_bits();
    let sign = ((bits >> 16) & 0x8000) as u16;
    let exponent = ((bits >> 23) & 0xff) as i32 - 127 + 15;
    let mantissa = ((bits >> 13) & 0x3ff) as u16;
    let half = if exponent <= 0 {
        sign
    } else if exponent >= 0x1f {
        sign | 0x7c00
    } else {
        sign | ((exponent as u16) << 10) | mantissa
    };
    half.to_le_bytes()
}

fn to_f32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

const ROWS: usize = 4096; // hidden
const COLS: usize = 11264; // intermediate

fn main() {
    let pools = PoolSizes {
        weights: 2 << 30,
        kv_cache: 16 << 20,
        activations: 256 << 20,
        kv_page_size: 256 << 10,
    };
    let cluster = Cluster::open(2, pools).expect("klaster");
    println!("P2P: {}", cluster.peer_access());
    let caps: Vec<DeviceCapability> = cluster.calibrate(QuantKind::Q8_0).expect("kalibracja");

    // Wagi Q8_0: 34 B na 32 wartosci, skale ustawione na male dodatnie f16.
    let row_bytes = (COLS / 32) * 34;
    let mut data = vec![0u8; ROWS * row_bytes];
    let mut seed = 0x9e37_79b9u32;
    for byte in data.iter_mut() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        *byte = (seed >> 24) as u8;
    }
    for row in 0..ROWS {
        for block in 0..COLS / 32 {
            let at = row * row_bytes + block * 34;
            data[at] = 0x00;
            data[at + 1] = 0x2C;
        }
    }

    // Wejscie: wymiar POSREDNI, bo to on jest podzielony.
    let x: Vec<u8> = (0..COLS)
        .flat_map(|i| {
            f16_bytes(((i % 13) as f32 - 6.0) * 0.05)
        })
        .collect();

    // Odniesienie: cala macierz na karcie 0.
    let dev0 = cluster.device(0).expect("karta 0");
    let w_all = dev0
        .device
        .alloc(data.len(), MemKind::Device, Pool::Weights)
        .expect("wagi odniesienia");
    dev0.device.write(&data, &w_all, 0).expect("zapis wag");
    let x_all = dev0
        .device
        .alloc(x.len(), MemKind::Device, Pool::Activations)
        .expect("wejscie odniesienia");
    dev0.device.write(&x, &x_all, 0).expect("zapis wejscia");
    let y_ref = dev0
        .device
        .alloc(ROWS * 4, MemKind::Device, Pool::Activations)
        .expect("wynik odniesienia");
    // Odniesienie musi wybierac kernel DOKLADNIE tak jak `Model::gemv`: dla
    // Q8_0 w zasiegu dp4a silnik kwantyzuje aktywacje do int8.
    dev0.kernels
        .gemv_q8_0_dp4a_out_f32(&y_ref, &w_all, &x_all, ROWS, COLS, &dev0.stream)
        .expect("gemv odniesienia");
    dev0.stream.synchronize().expect("sync");
    let mut reference = vec![0u8; ROWS * 4];
    dev0.device
        .read(&y_ref, 0, &mut reference)
        .expect("odczyt odniesienia");

    // Podzial kolumnowy.
    let shards = upload_column_split(
        &cluster,
        &caps,
        &data,
        ROWS,
        COLS,
        WorkKind::MemoryBound,
        BlockFormat::of(QuantKind::Q8_0).expect("format"),
    )
        .expect("podzial kolumnowy");
    for index in 0..cluster.len() {
        println!(
            "  dev{index}: kolumny {}..{}",
            shards.offset_of(index),
            shards.offset_of(index) + shards.cols_on(index)
        );
    }

    // Kazda karta dostaje SWOJ wycinek wejscia.
    let mut x_parts = Vec::new();
    let mut y_parts = Vec::new();
    for index in 0..cluster.len() {
        let entry = cluster.device(index).expect("karta");
        let count = shards.cols_on(index).max(1);
        let part = entry
            .device
            .alloc(count * 2, MemKind::Device, Pool::Activations)
            .expect("wycinek wejscia");
        if shards.cols_on(index) > 0 {
            let from = shards.offset_of(index) * 2;
            entry
                .device
                .write(&x[from..from + count * 2], &part, 0)
                .expect("zapis wycinka");
        }
        x_parts.push(part);
        y_parts.push(
            entry
                .device
                .alloc(ROWS * 4, MemKind::Device, Pool::Activations)
                .expect("suma czastkowa"),
        );
    }
    let y_full = dev0
        .device
        .alloc(ROWS * 4, MemKind::Device, Pool::Activations)
        .expect("wynik zbiorczy");
    let staging = dev0
        .device
        .alloc(ROWS * 4, MemKind::Device, Pool::Activations)
        .expect("bufor redukcji");

    gemv_q8_0_column_split(
        &cluster,
        &shards,
        &x_parts,
        &y_parts,
        &y_full,
        &staging,
        0,
        &dev0.stream,
    )
        .expect("gemv kolumnowy");
    cluster.synchronize().expect("sync klastra");
    let mut split = vec![0u8; ROWS * 4];
    dev0.device
        .read(&y_full, 0, &mut split)
        .expect("odczyt wyniku");

    let r = to_f32(&reference);
    let s = to_f32(&split);
    let mut identical = 0usize;
    let mut max_abs = 0f32;
    let (mut num, mut den) = (0f64, 0f64);
    for (a, b) in r.iter().zip(s.iter()) {
        if a == b {
            identical += 1;
        }
        let diff = (a - b) as f64;
        max_abs = max_abs.max(diff.abs() as f32);
        num += diff * diff;
        den += (*a as f64) * (*a as f64);
    }
    let l2 = (num / den.max(1e-12)).sqrt();
    println!(
        "zgodnosc: bitowo {identical}/{}, max |roznica| {max_abs:.6}, wzgledne L2 {l2:.2e}",
        r.len()
    );
    // Karty sumuja swoje zakresy osobno, wiec kolejnosc dodawania f32 jest inna
    // niz na jednej karcie — zgodnosc jest NUMERYCZNA, nie bitowa. Miara to
    // wzgledne L2 calego wektora, bo pojedyncze pola bliskie zeru daja wysoka
    // roznice wzgledna przy zaniedbywalnej bezwzglednej (tu 2,1e-4 przy
    // 3,1e-5 bezwzglednej).
    assert!(
        l2 < 1e-5,
        "podzial kolumnowy rozjechal sie z jednokartowym: L2 {l2:.2e}"
    );

    let mut split_time = f64::MAX;
    let mut whole_time = f64::MAX;
    for _ in 0..20 {
        let t0 = Instant::now();
        gemv_q8_0_column_split(
        &cluster,
        &shards,
        &x_parts,
        &y_parts,
        &y_full,
        &staging,
        0,
        &dev0.stream,
    )
            .expect("gemv kolumnowy");
        cluster.synchronize().expect("sync");
        split_time = split_time.min(t0.elapsed().as_secs_f64());

        let t1 = Instant::now();
        dev0.kernels
            .gemv_q8_0_dp4a_out_f32(&y_ref, &w_all, &x_all, ROWS, COLS, &dev0.stream)
            .expect("gemv odniesienia");
        dev0.stream.synchronize().expect("sync");
        whole_time = whole_time.min(t1.elapsed().as_secs_f64());
    }
    // UWAGA: te 0,87x to NIE jest werdykt o tensor parallel. Pojedyncza
    // macierz nie ma z czego oplacic wymiany — redukcja kosztuje ~11 us przy
    // 44 us liczenia. Zysk pojawia sie na CALEJ warstwie, gdzie `gate`/`up`
    // licza sie rownolegle po wierszach, a wymiana jest tylko JEDNA na warstwe:
    // `tp_layer_probe` mierzy tam 1,25x dla samego FFN.
    println!(
        "czas: jedna karta {:.1} us, dwie karty {:.1} us -> {:.2}x (jedna macierz)",
        whole_time * 1e6,
        split_time * 1e6,
        whole_time / split_time
    );
}
