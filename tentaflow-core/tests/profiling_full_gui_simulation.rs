// =============================================================================
// File: tests/profiling_full_gui_simulation.rs
// Opis: PRAWDZIWY E2E ktory symuluje dokladnie to co GUI robi:
//   1. Start session z duration_seconds=10 (jak user kliknie 'Start' z 10s
//      slider'em w mockup #01)
//   2. Czekaj 11 sekund - watchdog backend'u musi auto-stopnac sesje
//   3. Sprawdz ze summary.bin EXIST na dysku
//   4. Sprawdz ze manifest.json exist
//   5. Sprawdz ze list_sessions zwraca te sesje (nie filtrowana out)
//   6. Sprawdz ze read_report zwraca prawidlowy envelope V2
//   7. Walidacja calego flow - wszystko musi zakonczyc sie sukcesem
//
// To jest test KTOREGO mi brakowalo. Moje poprzednie testy uzywaly
// duration_seconds=0 + manual stop(handle) ktora pomijala auto-stop watchdog.
// User mial rację: nigdy nie sprawdzilem czy watchdog faktycznie pisze summary.
// =============================================================================

use std::sync::Arc;
use std::time::{Duration, Instant};

use tentaflow_core::profiling::{
    CollectorRegistry, MultiSourceSession, ParserRegistry, ProfileStorageV2,
};
use tentaflow_protocol::profiling::{
    GpuTargets, ProfileReportEnvelope, ProfileScope, ProfileSourceFlags, ProfileTarget,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn full_gui_flow_10s_auto_stop() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
    let registry = Arc::new(CollectorRegistry::discover());
    let parsers = Arc::new(ParserRegistry::default_registry());
    let session = MultiSourceSession::new(storage.clone(), registry.clone());

    let sources = ProfileSourceFlags(
        ProfileSourceFlags::CPU_UTIL
            | ProfileSourceFlags::RAM_USAGE
            | ProfileSourceFlags::DISK_IO
            | ProfileSourceFlags::NETWORK,
    );

    let scope = ProfileScope {
        sources,
        gpu_targets: GpuTargets::All,
        cpu_sampling_hz: 99,
        target: ProfileTarget::SystemWide,
        // KLUCZOWE: 10s auto-stop watchdog (jak GUI ustawia z duration slider).
        duration_seconds: 10,
        label: "gui-flow-10s".to_string(),
    };

    let session_id = "abcdef0123456789a1".to_string();
    let node_id = "node-gui-test".to_string();

    println!("=== KROK 1: Start sesji z duration_seconds=10 (auto-stop) ===");
    let t_start = Instant::now();
    let handle = Arc::clone(&session)
        .start(
            scope,
            node_id.clone(),
            session_id.clone(),
            "gui-flow-10s".to_string(),
            None,
            parsers,
        )
        .await
        .expect("start failed");
    let _ = handle; // SessionHandle - epoch jest private, ale watchdog go uzywa
    println!("  Session started.");
    println!("  is_active: {}", session.is_active().await);

    // Sprawdz ze active_info zwraca info.
    let info = session.active_info().await.expect("active_info should be Some");
    println!("  active_info.session_id={}", info.session_id);
    println!("  active_info.collectors_running={:?}", info.collectors_running);
    assert_eq!(info.session_id, session_id);
    assert!(!info.collectors_running.is_empty(), "min 1 collector active");

    // KROK 2: czekaj na auto-stop. 10s + 3s buffer.
    println!("\n=== KROK 2: Czekaj 13s na watchdog auto-stop ===");
    for sec in 1..=13 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let still_active = session.is_active().await;
        println!("  t+{sec}s: is_active={still_active}");
        // Jezeli watchdog odpalil przed time, mozemy break.
        if !still_active && sec >= 10 {
            println!("  Watchdog stopped session at t+{sec}s.");
            break;
        }
    }
    let elapsed = t_start.elapsed();
    println!("  Total elapsed: {:.1}s", elapsed.as_secs_f64());

    // KROK 3: po watchdog sesja musi byc nieaktywna.
    let still_active = session.is_active().await;
    println!("\n=== KROK 3: Po 13s session.is_active() == false? ===");
    println!("  is_active: {still_active}");
    assert!(
        !still_active,
        "BUG: sesja nadal aktywna po {:.1}s (duration=10s) - watchdog NIE auto-stop'ował!",
        elapsed.as_secs_f64()
    );

    // KROK 4: pliki na dysku. Dorzucamy delay zeby watchdog stop()
    // (running w background task) zdazyl ukonczyc write_session.
    println!("\n=== KROK 4: Pliki na dysku po auto-stop ===");
    println!("  Sleep 3s zeby watchdog write_session ukonczyl IO...");
    tokio::time::sleep(Duration::from_secs(3)).await;

    // List wszystko w tmp dir zeby zobaczyc co backend faktycznie zapisał.
    fn ls_recursive(dir: &std::path::Path, depth: usize) {
        if depth > 5 { return; }
        let Ok(rd) = std::fs::read_dir(dir) else { return; };
        for e in rd.flatten() {
            let p = e.path();
            let prefix = "  ".repeat(depth);
            let kind = if p.is_dir() { "DIR " } else { "FILE" };
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            println!("{}{} {} ({} bytes)", prefix, kind, p.display(), size);
            if p.is_dir() {
                ls_recursive(&p, depth + 1);
            }
        }
    }
    println!("  tmp tree:");
    ls_recursive(tmp.path(), 1);

    // ProfileStorageV2::new() dokleja "profiling" jako subdir storage root.
    let session_dir = tmp.path().join("profiling").join(&node_id).join(&session_id);
    println!("  expected session_dir = {}", session_dir.display());
    assert!(session_dir.exists(), "session dir nie istnieje");

    let manifest_path = session_dir.join("manifest.json");
    let summary_path = session_dir.join("summary.bin");
    println!("  manifest.json exists: {} ({} bytes)",
        manifest_path.exists(),
        std::fs::metadata(&manifest_path).map(|m| m.len()).unwrap_or(0));
    println!("  summary.bin  exists: {} ({} bytes)",
        summary_path.exists(),
        std::fs::metadata(&summary_path).map(|m| m.len()).unwrap_or(0));
    assert!(manifest_path.exists(), "BUG: manifest.json nie zostal zapisany przez auto-stop!");
    assert!(summary_path.exists(), "BUG: summary.bin nie zostal zapisany przez auto-stop!");

    // KROK 5: list_sessions zwraca nasza sesje.
    println!("\n=== KROK 5: storage.list_sessions zwraca sesje? ===");
    let list = storage.list_sessions(&node_id).await.expect("list_sessions failed");
    println!("  Sessions w liscie: {}", list.len());
    for s in &list {
        println!("    - {} ({} bytes)", s.session_id, s.size_bytes);
    }
    let found = list.iter().find(|s| s.session_id == session_id);
    assert!(found.is_some(), "BUG: nasza sesja {session_id} NIE w list_sessions!");

    // KROK 6: read_report zwraca prawidlowy envelope.
    println!("\n=== KROK 6: storage.read_report zwraca envelope ===");
    let envelope = storage
        .read_report(&node_id, &session_id)
        .await
        .expect("read_report failed");
    match envelope {
        ProfileReportEnvelope::V2(report) => {
            println!("  envelope: V2");
            println!("  session_id: {}", report.session_id);
            println!("  duration_ns: {} ({:.2}s)", report.duration_ns, report.duration_ns as f64 / 1e9);
            println!("  collectors: {}", report.collectors.len());
            println!("  events: {}", report.events.len());
            assert_eq!(report.session_id, session_id);
            assert!(
                report.duration_ns >= 8_000_000_000 && report.duration_ns <= 12_000_000_000,
                "BUG: duration_ns={} (expected ~10s, range 8-12s)",
                report.duration_ns
            );
            assert!(!report.collectors.is_empty(), "BUG: zero collectors!");
            assert!(!report.events.is_empty(), "BUG: zero events!");
        }
        ProfileReportEnvelope::V1Legacy(_) => {
            panic!("BUG: spodziewany envelope V2, dostalem V1Legacy");
        }
    }

    println!("\n=== FULL GUI FLOW: ALL 6 STEPS PASSED ===");
}
