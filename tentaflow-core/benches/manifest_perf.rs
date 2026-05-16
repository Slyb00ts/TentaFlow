// =============================================================================
// File: benches/manifest_perf.rs — M3.W13 manifest parse + validate bench
// =============================================================================
//
// Target from `notes/tentavision-plan.md` §17.8:
//   Manifest parse + validation  < 5 ms p99
//
// `parse_manifest_toml` does the full work:
//   1. `toml::from_str` over the manifest text.
//   2. Section walk: `[addon]`, `[[permission]]`, `[[tool]]`, `[[network_rule]]`,
//      `[[oauth_provider]]`, `[[alias]]`, `[[gate]]`, `[[vector_namespace]]`,
//      `[[flow_template]]`, `[[ui_component]]`, `[[uses_alias]]`,
//      `[[uses_model]]`, `[storage]`, `[resources]`, `[lifecycle]`, etc.
//   3. `validate_manifest_extensions` — uniqueness checks, enum guards,
//      signature format, semver `sdk_version`, storage/sql_backends coherence.
//
// We bench against `addons/test-addon/manifest.toml` (~120 lines, exercises
// permissions + tools + network rules + visibility — a realistic mid-size
// addon manifest).
//
// Run: `cargo bench --bench manifest_perf -- --quick --noplot`

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};

use tentaflow_core::addon::lifecycle::parse_manifest_toml;

const MANIFEST_SRC: &str = include_str!("../addons/test-addon/manifest.toml");

fn bench_manifest_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("manifest");

    // Sanity: ensure the manifest still parses before we start the timed loop —
    // a regression in the schema would otherwise show up as a panic mid-bench.
    parse_manifest_toml(MANIFEST_SRC).expect("test-addon manifest must parse");

    group.bench_function("parse_validate", |b| {
        b.iter(|| {
            let manifest = parse_manifest_toml(black_box(MANIFEST_SRC))
                .expect("manifest parse");
            black_box(manifest);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_manifest_parse);
criterion_main!(benches);
