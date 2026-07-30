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

    let bytes = 5120 * 2; // ukryty stan f16 jednego tokena
    let alloc = |index: usize| {
        cluster
            .device(index)
            .unwrap()
            .device
            .alloc(bytes, MemKind::Device, Pool::Activations)
            .unwrap()
    };
    let src = alloc(0);
    let dst = alloc(1);

    // Rozgrzewka, potem 200 wymian.
    for _ in 0..10 {
        cluster.exchange(0, &src, 0, 1, &dst, 0, bytes).unwrap();
    }
    cluster.synchronize().unwrap();
    let start = std::time::Instant::now();
    for _ in 0..200 {
        cluster.exchange(0, &src, 0, 1, &dst, 0, bytes).unwrap();
    }
    cluster.synchronize().unwrap();
    let per = start.elapsed().as_secs_f64() * 1e6 / 200.0;
    println!("wymiana {bytes} B: {per:.2} us");
    println!("65 warstw x 2 wymiany na token: {:.2} ms", per * 130.0 / 1000.0);
}
