// ===== File: cluster_peer.rs — peer-access probe for the local multi-GPU rig =====
// Sprawdza, czy karty w maszynie widzą się nawzajem bez powrotu do hosta, i mierzy
// koszt wymiany bloku wielkości ukrytego stanu. Ta liczba decyduje o kształcie
// tensor parallel: all-reduce po każdej warstwie płaci ją dwa razy na warstwę.

use forge_engine::cluster::Cluster;
use forge_hal::{Pool, PoolSizes};
use forge_types::MemKind;

#[test]
#[ignore = "wymaga co najmniej dwoch kart w maszynie"]
fn peer_access_and_exchange_cost() {
    let pools = PoolSizes::auto_from_free(4 << 30);
    let cluster = match Cluster::open(2, pools) {
        Ok(cluster) => cluster,
        Err(error) => {
            eprintln!("brak dwoch kart: {error}");
            return;
        }
    };
    println!("karty: {}", cluster.len());
    println!("peer access: {}", cluster.peer_access());

    // Rozmiary, które REALNIE występują w podziale: ukryty stan tokena,
    // projekcje DeltaNet i pełny wektor logitów. Jedna liczba nie wystarczy —
    // mała wymiana mierzy opóźnienie, duża przepustowość, a decyzja o tym, co
    // wolno dzielić, potrzebuje obu.
    let cap = 4 << 20;
    let alloc = |index: usize| {
        cluster
            .device(index)
            .unwrap()
            .device
            .alloc(cap, MemKind::Device, Pool::Activations)
            .unwrap()
    };
    let src = alloc(0);
    let dst = alloc(1);

    for &bytes in &[5120 * 2usize, 64 << 10, 512 << 10, 1 << 20, 4 << 20] {
        for _ in 0..10 {
            cluster.exchange(0, &src, 0, 1, &dst, 0, bytes).unwrap();
        }
        cluster.synchronize().unwrap();
        let iters = if bytes > (256 << 10) { 50 } else { 200 };
        let start = std::time::Instant::now();
        for _ in 0..iters {
            cluster.exchange(0, &src, 0, 1, &dst, 0, bytes).unwrap();
        }
        cluster.synchronize().unwrap();
        let per = start.elapsed().as_secs_f64() / iters as f64;
        println!(
            "wymiana {:>7} B: {:8.2} us, {:6.2} GB/s",
            bytes,
            per * 1e6,
            bytes as f64 / per / 1e9
        );
    }
}
