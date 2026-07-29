// Sprawdza kontekst klastra na REALNYCH kartach: otwarcie, P2P, wymianę i
// oczekiwanie na zdarzeniu. To jest fundament, na którym stoją wszystkie
// techniki podziału — jeśli tu coś nie działa, nie ma sensu iść dalej.
use forge_engine::cluster::Cluster;
use forge_hal::{Pool, PoolSizes};
use forge_types::MemKind;
use std::time::Instant;

fn main() {
    let pools = PoolSizes {
        weights: 256 << 20,
        kv_cache: 16 << 20,
        activations: 64 << 20,
        kv_page_size: 256 << 10,
    };
    let cluster = Cluster::open(2, pools).expect("otwarcie klastra");
    println!(
        "kart: {}, P2P miedzy kazda para: {}",
        cluster.len(),
        cluster.peer_access()
    );
    for (index, entry) in cluster.devices().iter().enumerate() {
        println!("  dev{index}: {}", entry.device.caps().name);
    }

    let bytes = 10 << 10;
    let src = cluster
        .device(0)
        .unwrap()
        .device
        .alloc(bytes, MemKind::Device, Pool::Activations)
        .expect("bufor");
    let dst = cluster
        .device(1)
        .unwrap()
        .device
        .alloc(bytes, MemKind::Device, Pool::Activations)
        .expect("bufor");

    // Pelny cykl tensor parallel: karta 0 liczy, przekazuje, karta 1 czeka.
    const ROUNDS: usize = 200;
    for _ in 0..8 {
        cluster.exchange(0, &src, 0, 1, &dst, 0, bytes).unwrap();
        cluster.wait_for(1, 0).unwrap();
    }
    cluster.synchronize().unwrap();
    let started = Instant::now();
    for _ in 0..ROUNDS {
        cluster.exchange(0, &src, 0, 1, &dst, 0, bytes).unwrap();
        cluster.wait_for(1, 0).unwrap();
    }
    cluster.synchronize().unwrap();
    println!(
        "cykl wymiany 10 KiB + oczekiwanie: {:.2} us",
        started.elapsed().as_secs_f64() / ROUNDS as f64 * 1e6
    );

    assert!(cluster.exchange(0, &src, 0, 0, &dst, 0, bytes).is_err());
    println!("wymiana na te sama karte odrzucona zgodnie z kontraktem");
}
