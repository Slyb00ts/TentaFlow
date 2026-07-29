// CALY blok FFN na dwoch kartach, z jedna wymiana na warstwe.
//
//   gate/up  — podzial po WIERSZACH: karta liczy swoj kawalek wymiaru
//              posredniego i od razu nakalda na niego SwiGLU, bez komunikacji,
//   down     — podzial po KOLUMNACH: zjada dokladnie ten kawalek i daje sume
//              czastkowa,
//   redukcja — JEDNA wymiana na cala warstwe.
//
// Pierwszym kryterium jest zgodnosc z tym samym blokiem policzonym w calosci na
// jednej karcie, dopiero potem czas. Ksztalty jak w FFN Bielika 7B.
use forge_engine::cluster::Cluster;
use forge_engine::multi_gpu::{DeviceCapability, WorkKind};
use forge_engine::tensor_parallel::{FfnWorkspace, ffn_forward_split, upload_ffn_split};
use forge_hal::{Pool, PoolSizes};
use forge_types::{MemKind, QuantKind};
use std::time::Instant;

const HIDDEN: usize = 4096;
const INTER: usize = 11264;

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

/// Wagi Q8_0 o powtarzalnej zawartosci i malych dodatnich skalach bloku.
fn synth(rows: usize, cols: usize, seed0: u32) -> Vec<u8> {
    let row_bytes = (cols / 32) * 34;
    let mut data = vec![0u8; rows * row_bytes];
    let mut seed = seed0;
    for byte in data.iter_mut() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        *byte = (seed >> 24) as u8;
    }
    for row in 0..rows {
        for block in 0..cols / 32 {
            let at = row * row_bytes + block * 34;
            data[at] = 0x00;
            data[at + 1] = 0x2C;
        }
    }
    data
}

fn main() {
    let pools = PoolSizes {
        weights: 4 << 30,
        kv_cache: 16 << 20,
        activations: 512 << 20,
        kv_page_size: 256 << 10,
    };
    let cluster = Cluster::open(2, pools).expect("klaster");
    let caps: Vec<DeviceCapability> = cluster.calibrate(QuantKind::Q8_0).expect("kalibracja");
    let act = forge_formats::FfnActivation::SiLU;

    let w_gate = synth(INTER, HIDDEN, 0x1111_1111);
    let w_up = synth(INTER, HIDDEN, 0x2222_2222);
    let w_down = synth(HIDDEN, INTER, 0x3333_3333);
    let x: Vec<u8> = (0..HIDDEN)
        .flat_map(|i| f16_bytes(((i % 17) as f32 - 8.0) * 0.03))
        .collect();

    // ---- odniesienie: caly blok na karcie 0 ----
    let dev0 = cluster.device(0).expect("karta 0");
    let alloc_w = |bytes: usize| {
        dev0.device
            .alloc(bytes, MemKind::Device, Pool::Weights)
            .expect("wagi")
    };
    let alloc_a = |bytes: usize| {
        dev0.device
            .alloc(bytes, MemKind::Device, Pool::Activations)
            .expect("aktywacje")
    };
    let g_all = alloc_w(w_gate.len());
    let u_all = alloc_w(w_up.len());
    let d_all = alloc_w(w_down.len());
    dev0.device.write(&w_gate, &g_all, 0).unwrap();
    dev0.device.write(&w_up, &u_all, 0).unwrap();
    dev0.device.write(&w_down, &d_all, 0).unwrap();
    let x_all = alloc_a(x.len());
    dev0.device.write(&x, &x_all, 0).unwrap();
    let gate_ref = alloc_a(INTER * 2);
    let up_ref = alloc_a(INTER * 2);
    let mid_ref = alloc_a(INTER * 2);
    let y_ref = alloc_a(HIDDEN * 4);

    // Odniesienie musi wybierac kernele DOKLADNIE tak jak `Model::gemv`: dla
    // Q8_0 w zasiegu dp4a silnik kwantyzuje aktywacje do int8. Liczenie tu
    // dokladniej dawaloby falszywy rozjazd z podzialem, ktory sluzy silnikowi.
    let whole = |dev: &forge_engine::cluster::ClusterDevice| {
        dev.kernels
            .gemv_q8_0_dp4a_f16(&gate_ref, &g_all, &x_all, INTER, HIDDEN, &dev.stream)
            .unwrap();
        dev.kernels
            .gemv_q8_0_dp4a_f16(&up_ref, &u_all, &x_all, INTER, HIDDEN, &dev.stream)
            .unwrap();
        dev.kernels
            .glu_mul_f16(act, &mid_ref, &gate_ref, &up_ref, INTER, &dev.stream)
            .unwrap();
        dev.kernels
            .gemv_q8_0_dp4a_out_f32(&y_ref, &d_all, &mid_ref, HIDDEN, INTER, &dev.stream)
            .unwrap();
    };
    whole(dev0);
    dev0.stream.synchronize().unwrap();
    let mut reference = vec![0u8; HIDDEN * 4];
    dev0.device.read(&y_ref, 0, &mut reference).unwrap();

    // ---- podzial: gate/up po wierszach, down po kolumnach ----
    let shards = upload_ffn_split(
        &cluster,
        &caps,
        &w_gate,
        &w_up,
        &w_down,
        HIDDEN,
        INTER,
        WorkKind::MemoryBound,
        None,
    )
    .expect("podzial FFN");
    let rows_plan: Vec<usize> = (0..cluster.len()).map(|i| shards.rows_on(i)).collect();

    let mut ws = FfnWorkspace {
        x: Vec::new(),
        gate: Vec::new(),
        up: Vec::new(),
        mid: Vec::new(),
        partial: Vec::new(),
    };
    for index in 0..cluster.len() {
        let entry = cluster.device(index).expect("karta");
        let rows = rows_plan[index].max(1);
        let mk = |n: usize| {
            entry
                .device
                .alloc(n, MemKind::Device, Pool::Activations)
                .unwrap()
        };
        let xc = mk(x.len());
        entry.device.write(&x, &xc, 0).unwrap();
        ws.x.push(xc);
        ws.gate.push(mk(rows * 2));
        ws.up.push(mk(rows * 2));
        ws.mid.push(mk(rows * 2));
        ws.partial.push(mk(HIDDEN * 4));
    }
    let y_full = alloc_a(HIDDEN * 4);
    let staging = alloc_a(HIDDEN * 4);

    let split_block = || {
        ffn_forward_split(
            &cluster,
            &shards,
            &ws,
            &y_full,
            &staging,
            HIDDEN,
            act,
            0,
        )
        .expect("blok FFN na kartach");
    };

    split_block();
    cluster.synchronize().unwrap();
    let mut split = vec![0u8; HIDDEN * 4];
    dev0.device.read(&y_full, 0, &mut split).unwrap();

    let r = to_f32(&reference);
    let s = to_f32(&split);
    let (mut num, mut den, mut max_abs) = (0f64, 0f64, 0f32);
    for (a, b) in r.iter().zip(s.iter()) {
        let diff = (a - b) as f64;
        max_abs = max_abs.max(diff.abs() as f32);
        num += diff * diff;
        den += (*a as f64) * (*a as f64);
    }
    let l2 = (num / den.max(1e-12)).sqrt();
    println!("podzial: {rows_plan:?} wierszy posrednich na karte");
    println!("zgodnosc bloku FFN: max |roznica| {max_abs:.6}, wzgledne L2 {l2:.2e}");
    assert!(l2 < 1e-5, "blok FFN rozjechal sie z jednokartowym");

    let mut split_time = f64::MAX;
    let mut whole_time = f64::MAX;
    for _ in 0..30 {
        let t0 = Instant::now();
        split_block();
        cluster.synchronize().unwrap();
        split_time = split_time.min(t0.elapsed().as_secs_f64());

        let t1 = Instant::now();
        whole(dev0);
        dev0.stream.synchronize().unwrap();
        whole_time = whole_time.min(t1.elapsed().as_secs_f64());
    }
    println!(
        "czas bloku FFN: jedna karta {:.1} us, dwie karty {:.1} us -> {:.2}x",
        whole_time * 1e6,
        split_time * 1e6,
        whole_time / split_time
    );
}
