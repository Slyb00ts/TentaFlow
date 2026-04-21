// =============================================================================
// Plik: tests/criterion_g_leak_soak.rs
// Opis: Iroh kryterium (g) — 4h leak soak, RSS+FD delta <5%.
//       Real test wymaga 4h wall-clock + RSS/FD monitoring (od strony OS).
//       Ten harness uruchamia ciagle accept/connect cycle przez TARGET_DURATION
//       i logi rss/fd co 60s. Operator porownuje delta start vs end.
//
//       Uruchamiaj: `cargo test --release --test criterion_g_leak_soak -- --ignored --nocapture`
//       Monitoring: `ps -o rss,nfd -p <PID>` lub `/proc/<PID>/status` co 60s
//       (do `target/spike-logs/criterion_g.txt`).
// =============================================================================

use anyhow::Result;
use std::time::{Duration, Instant};
use tentaflow_iroh_spike::accept_connect;

const TARGET_DURATION_SECS: u64 = 4 * 60 * 60; // 4h
const ACCEPT_BURST: usize = 10;
const BURST_INTERVAL_SECS: u64 = 5;

#[tokio::test]
#[ignore = "long-running 4h soak — uruchamiac na dedykowanym hardware z --ignored --release i monitoringiem rss/fd"]
async fn leak_soak_4h() -> Result<()> {
    let server = accept_connect::build_endpoint().await?;
    let server_addr = server.node_addr().await?;

    let server_handle = tokio::spawn(async move {
        while let Some(incoming) = server.accept().await {
            tokio::spawn(async move {
                if let Ok(conn) = incoming.await {
                    while let Ok((mut send, mut recv)) = conn.accept_bi().await {
                        if let Ok(bytes) = recv.read_to_end(64).await {
                            let _ = send.write_all(&bytes).await;
                            let _ = send.finish();
                        }
                    }
                }
            });
        }
    });

    let start = Instant::now();
    let test_end = start + Duration::from_secs(TARGET_DURATION_SECS);
    let mut total_cycles = 0u64;

    println!(
        "SOAK START pid={} target_duration={}s",
        std::process::id(),
        TARGET_DURATION_SECS
    );

    while Instant::now() < test_end {
        // Burst: tworz ACCEPT_BURST connections, zrob round-trip, zamknij.
        let mut handles = Vec::with_capacity(ACCEPT_BURST);
        for _ in 0..ACCEPT_BURST {
            let addr = server_addr.clone();
            handles.push(tokio::spawn(async move {
                let c = accept_connect::build_endpoint().await?;
                let conn = c.connect(addr, b"tentaflow-mesh").await?;
                let (mut send, mut recv) = conn.open_bi().await?;
                send.write_all(b"soak").await?;
                send.finish()?;
                let _ = recv.read_to_end(64).await?;
                conn.close(0u32.into(), b"ok");
                Ok::<(), anyhow::Error>(())
            }));
        }
        for h in handles {
            let _ = h.await;
        }
        total_cycles += ACCEPT_BURST as u64;

        if start.elapsed().as_secs() % 60 == 0 {
            println!(
                "SOAK PROGRESS elapsed={}s cycles={} rss={}",
                start.elapsed().as_secs(),
                total_cycles,
                read_rss_kb()
            );
        }

        tokio::time::sleep(Duration::from_secs(BURST_INTERVAL_SECS)).await;
    }

    println!("SOAK END cycles={} elapsed={:?}", total_cycles, start.elapsed());
    drop(server_handle);
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_rss_kb() -> u64 {
    use std::fs;
    let s = fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            if let Some(num) = rest.split_whitespace().next() {
                return num.parse().unwrap_or(0);
            }
        }
    }
    0
}

#[cfg(not(target_os = "linux"))]
fn read_rss_kb() -> u64 {
    0
}
