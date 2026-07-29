// Mierzy REALNY koszt wymiany aktywacji między dwiema kartami: opóźnienie małej
// kopii i pasmo dużej, przez P2P. Planer tensor parallel opiera na tych
// liczbach decyzję, czy dana para kart uniesie wspólne liczenie warstwy.
use forge_hal::{Pool, PoolSizes, gpu};
use forge_types::MemKind;
use std::time::Instant;

fn main() {
    let pools = PoolSizes {
        weights: 256 << 20,
        kv_cache: 16 << 20,
        activations: 192 << 20,
        kv_page_size: 256 << 10,
    };
    let devices: Vec<_> = (0..2)
        .map(|o| gpu::open(o, pools).expect("otwarcie karty"))
        .collect();
    for (index, device) in devices.iter().enumerate() {
        let peer = 1 - index;
        match device.enable_peer_access(peer) {
            Ok(()) => println!("dev{index} -> dev{peer}: P2P otwarte"),
            Err(error) => {
                println!("dev{index} -> dev{peer}: P2P niedostępne ({error})");
                return;
            }
        }
    }

    let stream = devices[0].create_stream().expect("strumień");
    for &bytes in &[10usize << 10, 1 << 20, 64 << 20] {
        let src = devices[0]
            .alloc(bytes, MemKind::Device, Pool::Activations)
            .expect("bufor źródłowy");
        let dst = devices[1]
            .alloc(bytes, MemKind::Device, Pool::Activations)
            .expect("bufor docelowy");
        for _ in 0..8 {
            devices[0].copy(&src, 0, &dst, 0, bytes, &stream).expect("kopia");
        }
        stream.synchronize().expect("sync");
        const ITERS: usize = 200;
        let started = Instant::now();
        for _ in 0..ITERS {
            devices[0].copy(&src, 0, &dst, 0, bytes, &stream).expect("kopia");
        }
        stream.synchronize().expect("sync");
        let seconds = started.elapsed().as_secs_f64() / ITERS as f64;
        println!(
            "{:>8} KiB: {:>8.2} us, {:>6.1} GB/s",
            bytes >> 10,
            seconds * 1e6,
            bytes as f64 / seconds / 1e9
        );
    }

    // Tensor parallel wymaga, żeby druga karta CZEKAŁA na wynik pierwszej bez
    // powrotu do hosta — inaczej narzut zjada cały zysk z podziału.
    let other = devices[1].create_stream().expect("strumień drugiej karty");
    let event = devices[0].create_event().expect("zdarzenie");
    let bytes = 10 << 10;
    let src = devices[0]
        .alloc(bytes, MemKind::Device, Pool::Activations)
        .expect("bufor");
    let dst = devices[1]
        .alloc(bytes, MemKind::Device, Pool::Activations)
        .expect("bufor");
    const ROUNDS: usize = 200;
    for _ in 0..8 {
        devices[0].copy(&src, 0, &dst, 0, bytes, &stream).expect("kopia");
        devices[0].record_event(&event, &stream).expect("zapis zdarzenia");
        devices[1].wait_event(&other, &event).expect("oczekiwanie");
    }
    stream.synchronize().expect("sync");
    other.synchronize().expect("sync");
    let started = Instant::now();
    for _ in 0..ROUNDS {
        devices[0].copy(&src, 0, &dst, 0, bytes, &stream).expect("kopia");
        devices[0].record_event(&event, &stream).expect("zapis zdarzenia");
        devices[1].wait_event(&other, &event).expect("oczekiwanie");
    }
    stream.synchronize().expect("sync");
    other.synchronize().expect("sync");
    let seconds = started.elapsed().as_secs_f64() / ROUNDS as f64;
    println!(
        "wymiana 10 KiB + synchronizacja na zdarzeniu: {:>8.2} us",
        seconds * 1e6
    );
}
