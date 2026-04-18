// =============================================================================
// Plik: tests/criterion_f_throughput.rs
// Opis: Iroh kryterium (f) — throughput 100 conn / 1000 msg/s / 30min sustained.
//       Real test wymaga 30-min wall-clock + 100 souls + load measurement.
//       Ten plik to harness do uruchamiania ad-hoc — kompilauje sie i ma
//       ignored test ktory mozna odpalic na dedykowanym hardware.
//
//       Uruchamiaj: `cargo test --release --test criterion_f_throughput -- --ignored --nocapture`
//
//       Output do `target/spike-logs/criterion_f.txt` (nieautomatyzowane —
//       operator zapisuje rezultaty manualnie do CRITERIA.md).
// =============================================================================

use anyhow::Result;
use std::time::{Duration, Instant};
use tentaflow_iroh_spike::accept_connect;

const TARGET_CONNECTIONS: usize = 100;
const TARGET_MESSAGES_PER_SECOND: u64 = 1_000;
const TEST_DURATION_SECS: u64 = 30 * 60; // 30 min

#[tokio::test]
#[ignore = "long-running 30 min throughput test — uruchamiac na dedykowanym hardware z --ignored --release"]
async fn sustained_throughput_30min() -> Result<()> {
    let server = accept_connect::build_endpoint().await?;
    let server_addr = server.node_addr().await?;

    // Server task: accept polaczen w petli, echo bytes.
    let server_handle = tokio::spawn(async move {
        let mut connections = Vec::new();
        while let Some(incoming) = server.accept().await {
            let conn = match incoming.await {
                Ok(c) => c,
                Err(_) => continue,
            };
            connections.push(tokio::spawn(async move {
                while let Ok((mut send, mut recv)) = conn.accept_bi().await {
                    if let Ok(bytes) = recv.read_to_end(1024).await {
                        let _ = send.write_all(&bytes).await;
                        let _ = send.finish();
                    }
                }
            }));
            if connections.len() >= TARGET_CONNECTIONS * 2 {
                break;
            }
        }
        for h in connections {
            let _ = h.await;
        }
    });

    let start = Instant::now();
    let mut total_messages = 0u64;
    let mut clients = Vec::new();

    for _ in 0..TARGET_CONNECTIONS {
        let client = accept_connect::build_endpoint().await?;
        let conn = client.connect(server_addr.clone(), b"tentaflow-mesh").await?;
        clients.push((client, conn));
    }

    let interval = Duration::from_micros(1_000_000 / TARGET_MESSAGES_PER_SECOND);
    let test_end = start + Duration::from_secs(TEST_DURATION_SECS);

    while Instant::now() < test_end {
        for (_, conn) in &clients {
            let (mut send, mut recv) = conn.open_bi().await?;
            send.write_all(b"x").await?;
            send.finish()?;
            let _ = recv.read_to_end(8).await?;
            total_messages += 1;
        }
        tokio::time::sleep(interval).await;
    }

    let elapsed = start.elapsed();
    let actual_rate = total_messages as f64 / elapsed.as_secs_f64();
    println!("THROUGHPUT: {} msgs in {:?} = {:.1} msg/s", total_messages, elapsed, actual_rate);
    println!("TARGET: {} msg/s, ACTUAL: {:.1} msg/s", TARGET_MESSAGES_PER_SECOND, actual_rate);

    drop(clients);
    let _ = tokio::time::timeout(Duration::from_secs(5), server_handle).await;

    assert!(
        actual_rate >= (TARGET_MESSAGES_PER_SECOND as f64) * 0.9,
        "throughput {} msg/s < 90% of target {}",
        actual_rate,
        TARGET_MESSAGES_PER_SECOND
    );
    Ok(())
}
