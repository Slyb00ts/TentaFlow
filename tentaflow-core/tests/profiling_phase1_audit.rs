// =============================================================================
// File: tests/profiling_phase1_audit.rs
// Opis: Phase 1 audit profilowania multi-source. Uruchamia 3-sekundowa sesje
//       z REALNYMI collectorami systemowymi (CPU util, RAM, Disk - bez sudo)
//       i drukuje pelen breakdown ProfileReportV2: ile events per kategorie,
//       ile interned names/frames, czy zapis na dysk + odczyt round-trip
//       dziala. Porownujemy z tym czego oczekuje GUI (mockup 04-12).
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use tentaflow_core::profiling::{
    CollectorRegistry, MultiSourceSession, ParserRegistry, ProfileStorageV2,
};
use tentaflow_protocol::profiling::{
    EventPayload, GpuTargets, ProfileScope, ProfileSourceFlags, ProfileTarget,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn audit_real_session_3s() {
    let tmp = tempfile::tempdir().unwrap();
    let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
    let registry = Arc::new(CollectorRegistry::discover());
    let parsers = Arc::new(ParserRegistry::default_registry());
    let session = MultiSourceSession::new(storage.clone(), registry.clone());

    // Wszystkie sources nie wymagajace sudo. RAM_BANDWIDTH (uncore IMC) i POWER
    // (RAPL) wymagaja sudo - pomijamy, bo test ma byc reproducible bez root.
    let sources = ProfileSourceFlags(
        ProfileSourceFlags::CPU_UTIL
            | ProfileSourceFlags::RAM_USAGE
            | ProfileSourceFlags::DISK_IO
            | ProfileSourceFlags::GPU
            | ProfileSourceFlags::NETWORK,
    );

    let scope = ProfileScope {
        sources,
        gpu_targets: GpuTargets::All,
        cpu_sampling_hz: 99,
        target: ProfileTarget::SystemWide,
        duration_seconds: 0,
        label: "phase1-audit".to_string(),
    };

    println!("=== PHASE 1 AUDIT: starting 3s multi-source profile session ===");
    println!("Discovered collectors: {}", registry.len());
    for c in registry.all() {
        println!("  - {}", c.id());
    }

    let session_id = "abcdef0123456789".to_string();
    let handle = Arc::clone(&session)
        .start(
            scope,
            "node-phase1".to_string(),
            session_id.clone(),
            "phase1-audit".to_string(),
            None,
            parsers,
        )
        .await
        .expect("start failed");

    println!("Session started, sleeping 3.5s for auto-collection...");
    tokio::time::sleep(Duration::from_millis(3500)).await;

    let report = session.stop(handle).await.expect("stop failed");

    println!("\n=== ASSEMBLED ProfileReportV2 ===");
    println!("schema_version: {}", report.schema_version);
    println!("session_id: {}", report.session_id);
    println!("node_id: {}", report.node_id);
    println!("duration_ns: {} ({:.2}s)", report.duration_ns, report.duration_ns as f64 / 1e9);
    println!("collectors: {}", report.collectors.len());
    for c in &report.collectors {
        println!(
            "  {} status={:?} samples={} raw_size={}B duration_ns={} cat={:?}",
            c.id, c.status, c.samples_collected, c.raw_size_bytes, c.duration_ns, c.primary_category
        );
    }
    println!("frames: {}", report.frames.len());
    println!("stacks: {}", report.stacks.len());
    println!("names: {}", report.names.len());
    println!("warnings: {:?}", report.warnings);
    println!("drift_report: {:?}", report.drift_report);

    println!("\n=== EVENTS BREAKDOWN BY PAYLOAD VARIANT ===");
    let mut counts = std::collections::BTreeMap::<&'static str, usize>::new();
    let mut categories = std::collections::BTreeMap::<String, usize>::new();
    for e in &report.events {
        let key = match &e.payload {
            EventPayload::CpuSample { .. } => "CpuSample",
            EventPayload::CpuCounter { .. } => "CpuCounter",
            EventPayload::CpuUtil { .. } => "CpuUtil",
            EventPayload::RamSample { .. } => "RamSample",
            EventPayload::RamBandwidth { .. } => "RamBandwidth",
            EventPayload::DiskIoBurst { .. } => "DiskIoBurst",
            EventPayload::GpuKernel { .. } => "GpuKernel",
            EventPayload::GpuApiCall { .. } => "GpuApiCall",
            EventPayload::GpuUtilSample { .. } => "GpuUtilSample",
            EventPayload::GpuMemSample { .. } => "GpuMemSample",
            EventPayload::GpuMemTransfer { .. } => "GpuMemTransfer",
            EventPayload::PowerSample { .. } => "PowerSample",
            EventPayload::NvtxRange { .. } => "NvtxRange",
            EventPayload::NetworkSample { .. } => "NetworkSample",
            EventPayload::Custom { .. } => "Custom",
        };
        *counts.entry(key).or_insert(0) += 1;
        *categories.entry(format!("{:?}", e.category)).or_insert(0) += 1;
    }
    for (k, v) in &counts {
        println!("  {:20} {}", k, v);
    }
    println!("\n=== EVENTS BY CATEGORY ===");
    for (k, v) in &categories {
        println!("  {:20} {}", k, v);
    }

    // Sample of first few events - co realnie wyglada w payload.
    println!("\n=== FIRST 10 EVENTS (raw preview) ===");
    for (i, e) in report.events.iter().take(10).enumerate() {
        println!(
            "  [{}] cat={:?} t_start={}ns t_end={}ns lane={} payload={:?}",
            i, e.category, e.t_start_ns, e.t_end_ns, e.lane_hint, e.payload
        );
    }

    // Round-trip via storage.
    println!("\n=== STORAGE ROUND-TRIP ===");
    let manifest = storage
        .read_manifest("node-phase1", &session_id)
        .await
        .expect("read_manifest");
    println!("manifest.size_bytes: {}", manifest.size_bytes);
    println!("manifest.kind: {:?}", manifest.kind);
    println!("manifest.collectors_used: {}", manifest.collectors_used.len());

    let envelope = storage
        .read_report("node-phase1", &session_id)
        .await
        .expect("read_report");
    match envelope {
        tentaflow_protocol::profiling::ProfileReportEnvelope::V2(r) => {
            println!("envelope: V2 ({} events, schema={})", r.events.len(), r.schema_version);
            assert_eq!(r.events.len(), report.events.len());
        }
        tentaflow_protocol::profiling::ProfileReportEnvelope::V1Legacy(_) => {
            panic!("expected V2 envelope, got V1Legacy");
        }
    }

    // Sanity: report ma JAKIES wydarzenia. Jesli 0 - to znaczy ze zaden collector
    // nie zadzialal i to jest bug do zgloszenia.
    assert!(
        !report.events.is_empty(),
        "BUG: 0 events collected. Sprawdz czy collectors faktycznie dzialaja."
    );

    println!("\n=== PHASE 1 AUDIT COMPLETE ===");
}
