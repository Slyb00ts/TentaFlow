// =============================================================================
// File: benches/canonical_bench.rs
// Desc: Criterion benchmarks for validate_canonical across realistic payload
//       sizes that mimic addon UI messages (envelope = array(2) [u16 tag, body]).
// =============================================================================

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use tentaflow_sdk_spec::validate_canonical;

// ---------------------------------------------------------------------------
// Canonical CBOR encoding helpers — deterministic, minimum-width, sorted keys.
// ---------------------------------------------------------------------------

fn encode_uint(buf: &mut Vec<u8>, major: u8, val: u64) {
    let hi = major << 5;
    if val <= 23 {
        buf.push(hi | val as u8);
    } else if val <= 0xFF {
        buf.push(hi | 24);
        buf.push(val as u8);
    } else if val <= 0xFFFF {
        buf.push(hi | 25);
        buf.extend_from_slice(&(val as u16).to_be_bytes());
    } else if val <= 0xFFFF_FFFF {
        buf.push(hi | 26);
        buf.extend_from_slice(&(val as u32).to_be_bytes());
    } else {
        buf.push(hi | 27);
        buf.extend_from_slice(&val.to_be_bytes());
    }
}

fn encode_u16(buf: &mut Vec<u8>, v: u16) {
    encode_uint(buf, 0, v as u64);
}

fn encode_bool(buf: &mut Vec<u8>, v: bool) {
    buf.push(if v { 0xF5 } else { 0xF4 });
}

fn encode_null(buf: &mut Vec<u8>) {
    buf.push(0xF6);
}

fn encode_f64(buf: &mut Vec<u8>, v: f64) {
    buf.push(0xFB);
    buf.extend_from_slice(&v.to_bits().to_be_bytes());
}

fn encode_tstr(buf: &mut Vec<u8>, s: &str) {
    encode_uint(buf, 3, s.len() as u64);
    buf.extend_from_slice(s.as_bytes());
}

fn encode_bstr(buf: &mut Vec<u8>, data: &[u8]) {
    encode_uint(buf, 2, data.len() as u64);
    buf.extend_from_slice(data);
}

fn encode_array_head(buf: &mut Vec<u8>, n: u64) {
    encode_uint(buf, 4, n);
}

fn encode_map_head(buf: &mut Vec<u8>, n: u64) {
    encode_uint(buf, 5, n);
}

// ---------------------------------------------------------------------------
// Payload builders — each returns a Vec<u8> of canonical CBOR that mimics
// a real addon UI protocol message: array(2) [ u16 tag, body_map ].
// ---------------------------------------------------------------------------

/// ~100 bytes — a single small component (e.g. tf-button render).
fn build_small_payload() -> Vec<u8> {
    let mut buf = Vec::with_capacity(160);
    encode_array_head(&mut buf, 2);
    encode_u16(&mut buf, 0x0101);

    // body map — 7 integer keys, sorted ascending
    encode_map_head(&mut buf, 7);

    // key 0 -> slot_id string
    encode_u16(&mut buf, 0);
    encode_tstr(&mut buf, "btn-confirm-action");

    // key 1 -> label
    encode_u16(&mut buf, 1);
    encode_tstr(&mut buf, "Confirm");

    // key 2 -> enabled flag
    encode_u16(&mut buf, 2);
    encode_bool(&mut buf, true);

    // key 3 -> variant
    encode_u16(&mut buf, 3);
    encode_tstr(&mut buf, "primary");

    // key 4 -> icon
    encode_u16(&mut buf, 4);
    encode_tstr(&mut buf, "check-circle");

    // key 5 -> tooltip
    encode_u16(&mut buf, 5);
    encode_tstr(&mut buf, "Click to confirm the pending operation");

    // key 6 -> aria / a11y map
    encode_u16(&mut buf, 6);
    encode_map_head(&mut buf, 2);
    encode_u16(&mut buf, 0);
    encode_tstr(&mut buf, "button");
    encode_u16(&mut buf, 1);
    encode_tstr(&mut buf, "Confirm pending operation");

    buf
}

/// ~2 KB — a panel section with several components (toolbar + table rows).
fn build_medium_payload() -> Vec<u8> {
    let mut buf = Vec::with_capacity(2200);
    encode_array_head(&mut buf, 2);
    encode_u16(&mut buf, 0x0300); // panel-section tag

    // body map with 5 keys
    encode_map_head(&mut buf, 5);

    // key 0 -> panel_id
    encode_u16(&mut buf, 0);
    encode_tstr(&mut buf, "contacts-list-section");

    // key 1 -> title
    encode_u16(&mut buf, 1);
    encode_tstr(&mut buf, "Active contacts");

    // key 2 -> toolbar (nested component)
    encode_u16(&mut buf, 2);
    encode_map_head(&mut buf, 3);
    encode_u16(&mut buf, 0);
    encode_tstr(&mut buf, "toolbar-main");
    encode_u16(&mut buf, 1);
    encode_tstr(&mut buf, "Contacts toolbar");
    encode_u16(&mut buf, 2);
    // actions array — 4 action maps
    encode_array_head(&mut buf, 4);
    for (i, label) in ["New", "Edit", "Delete", "Export"].iter().enumerate() {
        encode_map_head(&mut buf, 3);
        encode_u16(&mut buf, 0);
        encode_tstr(&mut buf, &format!("action-{i}"));
        encode_u16(&mut buf, 1);
        encode_tstr(&mut buf, label);
        encode_u16(&mut buf, 2);
        encode_bool(&mut buf, i != 2); // delete disabled
    }

    // key 3 -> columns definition
    encode_u16(&mut buf, 3);
    let columns = ["Name", "Email", "Phone", "Company", "Role", "Status"];
    encode_array_head(&mut buf, columns.len() as u64);
    for (i, col) in columns.iter().enumerate() {
        encode_map_head(&mut buf, 3);
        encode_u16(&mut buf, 0);
        encode_tstr(&mut buf, &format!("col-{i}"));
        encode_u16(&mut buf, 1);
        encode_tstr(&mut buf, col);
        encode_u16(&mut buf, 2);
        encode_bool(&mut buf, true); // sortable
    }

    // key 4 -> rows — 12 rows of data
    encode_u16(&mut buf, 4);
    encode_array_head(&mut buf, 12);
    for row_idx in 0u16..12 {
        encode_map_head(&mut buf, 6);
        for col_idx in 0u16..6 {
            encode_u16(&mut buf, col_idx);
            encode_tstr(
                &mut buf,
                &format!("row-{row_idx}-cell-{col_idx}-value-data"),
            );
        }
    }

    buf
}

/// ~10 KB — full panel with header, multiple sections, nested components.
fn build_large_payload() -> Vec<u8> {
    let mut buf = Vec::with_capacity(12_000);
    encode_array_head(&mut buf, 2);
    encode_u16(&mut buf, 0x0400); // full panel tag

    // top-level body: map with 6 keys
    encode_map_head(&mut buf, 6);

    // key 0 -> panel id
    encode_u16(&mut buf, 0);
    encode_tstr(&mut buf, "company-detail-panel");

    // key 1 -> header section
    encode_u16(&mut buf, 1);
    encode_map_head(&mut buf, 5);
    encode_u16(&mut buf, 0);
    encode_tstr(&mut buf, "header-section");
    encode_u16(&mut buf, 1);
    encode_tstr(&mut buf, "Acme Corporation International Ltd.");
    encode_u16(&mut buf, 2);
    encode_tstr(&mut buf, "Active customer since 2019");
    encode_u16(&mut buf, 3);
    encode_bool(&mut buf, true);
    encode_u16(&mut buf, 4);
    encode_tstr(&mut buf, "company");

    // key 2 -> key-value fields (stat group)
    encode_u16(&mut buf, 2);
    let kv_fields: &[(&str, &str)] = &[
        ("NIP", "1234567890"),
        ("REGON", "123456789"),
        ("KRS", "0000123456"),
        ("VAT Status", "Active"),
        ("Address", "ul. Warszawska 42, 00-001 Warszawa"),
        ("Phone", "+48 22 123 45 67"),
        ("Email", "contact@acme-corp.example.com"),
        ("Website", "https://acme-corp.example.com"),
        ("Industry", "Software Development"),
        ("Employees", "250"),
        ("Founded", "2005-03-15"),
        ("Legal Form", "Spolka z ograniczona odpowiedzialnoscia"),
        ("Capital", "500000.00 PLN"),
        ("Tax Office", "Urzad Skarbowy Warszawa-Centrum"),
        ("Bank Account", "PL61 1090 1014 0000 0712 1981 2874"),
    ];
    encode_array_head(&mut buf, kv_fields.len() as u64);
    for (k, v) in kv_fields {
        encode_map_head(&mut buf, 2);
        encode_u16(&mut buf, 0);
        encode_tstr(&mut buf, k);
        encode_u16(&mut buf, 1);
        encode_tstr(&mut buf, v);
    }

    // key 3 -> persons table (50 rows, 5 cols)
    encode_u16(&mut buf, 3);
    encode_map_head(&mut buf, 2);
    encode_u16(&mut buf, 0);
    {
        let cols = ["Name", "Position", "Email", "Phone", "Active"];
        encode_array_head(&mut buf, cols.len() as u64);
        for (i, c) in cols.iter().enumerate() {
            encode_map_head(&mut buf, 2);
            encode_u16(&mut buf, 0);
            encode_tstr(&mut buf, &format!("pcol-{i}"));
            encode_u16(&mut buf, 1);
            encode_tstr(&mut buf, c);
        }
    }
    encode_u16(&mut buf, 1);
    encode_array_head(&mut buf, 50);
    for r in 0u16..50 {
        encode_map_head(&mut buf, 5);
        encode_u16(&mut buf, 0);
        encode_tstr(&mut buf, &format!("Jan Kowalski-{r}"));
        encode_u16(&mut buf, 1);
        encode_tstr(&mut buf, "Senior Software Engineer");
        encode_u16(&mut buf, 2);
        encode_tstr(&mut buf, &format!("jan.kowalski.{r}@acme-corp.example.com"));
        encode_u16(&mut buf, 3);
        encode_tstr(
            &mut buf,
            &format!("+48 600 {:03} {:03}", r * 7 % 1000, r * 13 % 1000),
        );
        encode_u16(&mut buf, 4);
        encode_bool(&mut buf, r % 5 != 0);
    }

    // key 4 -> timeline events
    encode_u16(&mut buf, 4);
    encode_array_head(&mut buf, 30);
    for ev in 0u16..30 {
        encode_map_head(&mut buf, 5);
        encode_u16(&mut buf, 0);
        encode_tstr(&mut buf, &format!("evt-{ev:03}"));
        encode_u16(&mut buf, 1);
        encode_tstr(&mut buf, &format!("2025-06-{:02}T10:30:00Z", (ev % 28) + 1));
        encode_u16(&mut buf, 2);
        encode_tstr(
            &mut buf,
            "Updated contact information for employee record in the system database",
        );
        encode_u16(&mut buf, 3);
        encode_tstr(&mut buf, "admin@acme-corp.example.com");
        encode_u16(&mut buf, 4);
        encode_tstr(&mut buf, &format!("audit-ref-{ev:06}"));
    }

    // key 5 -> financial stats with f64 values
    encode_u16(&mut buf, 5);
    encode_map_head(&mut buf, 8);
    encode_u16(&mut buf, 0);
    encode_f64(&mut buf, 1_250_000.50);
    encode_u16(&mut buf, 1);
    encode_f64(&mut buf, 875_432.75);
    encode_u16(&mut buf, 2);
    encode_f64(&mut buf, 0.12);
    encode_u16(&mut buf, 3);
    encode_f64(&mut buf, 42_000.0);
    encode_u16(&mut buf, 4);
    encode_f64(&mut buf, 18_750.25);
    encode_u16(&mut buf, 5);
    encode_f64(&mut buf, 6.75);
    encode_u16(&mut buf, 6);
    encode_null(&mut buf);
    encode_u16(&mut buf, 7);
    encode_f64(&mut buf, 99_999.99);

    buf
}

/// ~50 KB — batch of multiple full panels (simulates a bulk slot update).
fn build_very_large_payload() -> Vec<u8> {
    let mut buf = Vec::with_capacity(55_000);
    encode_array_head(&mut buf, 2);
    encode_u16(&mut buf, 0x0500); // batch tag

    // body: map with 2 keys — batch_id + panels array
    encode_map_head(&mut buf, 2);

    // key 0 -> batch_id
    encode_u16(&mut buf, 0);
    encode_tstr(&mut buf, "batch-panel-update-2025-06-01");

    // key 1 -> array of panel sections
    encode_u16(&mut buf, 1);

    let panel_count: u64 = 11;
    encode_array_head(&mut buf, panel_count);

    for panel_idx in 0..panel_count as u16 {
        // Each panel: map with 6 keys
        encode_map_head(&mut buf, 6);

        // key 0 -> panel_id
        encode_u16(&mut buf, 0);
        encode_tstr(&mut buf, &format!("panel-section-{panel_idx:03}"));

        // key 1 -> title
        encode_u16(&mut buf, 1);
        encode_tstr(
            &mut buf,
            &format!("Dashboard widget {panel_idx}: Quarterly performance"),
        );

        // key 2 -> stats (4 stat items with f64)
        encode_u16(&mut buf, 2);
        encode_array_head(&mut buf, 4);
        for s in 0u16..4 {
            encode_map_head(&mut buf, 3);
            encode_u16(&mut buf, 0);
            encode_tstr(&mut buf, &format!("stat-{panel_idx}-{s}"));
            encode_u16(&mut buf, 1);
            encode_tstr(&mut buf, &format!("Metric {s}"));
            encode_u16(&mut buf, 2);
            encode_f64(&mut buf, (panel_idx as f64) * 1000.0 + (s as f64) * 42.5);
        }

        // key 3 -> table rows (15 rows, 8 cells each)
        encode_u16(&mut buf, 3);
        encode_array_head(&mut buf, 15);
        for row in 0u16..15 {
            encode_map_head(&mut buf, 8);
            for col in 0u16..8 {
                encode_u16(&mut buf, col);
                encode_tstr(
                    &mut buf,
                    &format!("panel-{panel_idx:02}-row-{row:02}-col-{col}-cell-data"),
                );
            }
        }

        // key 4 -> metadata (bstr blob for binary payload)
        encode_u16(&mut buf, 4);
        let blob: Vec<u8> = (0u8..128).collect();
        encode_bstr(&mut buf, &blob);

        // key 5 -> nested component tree
        encode_u16(&mut buf, 5);
        encode_map_head(&mut buf, 3);
        encode_u16(&mut buf, 0);
        encode_tstr(&mut buf, &format!("footer-{panel_idx:03}"));
        encode_u16(&mut buf, 1);
        encode_array_head(&mut buf, 3);
        for btn in 0u16..3 {
            encode_map_head(&mut buf, 2);
            encode_u16(&mut buf, 0);
            encode_tstr(&mut buf, &format!("btn-{panel_idx}-{btn}"));
            encode_u16(&mut buf, 1);
            encode_bool(&mut buf, btn == 0);
        }
        encode_u16(&mut buf, 2);
        encode_f64(&mut buf, panel_idx as f64 * 100.0 + 0.5);
    }

    buf
}

// ---------------------------------------------------------------------------
// Benchmark group
// ---------------------------------------------------------------------------

fn bench_validate_canonical(c: &mut Criterion) {
    let small = build_small_payload();
    let medium = build_medium_payload();
    let large = build_large_payload();
    let very_large = build_very_large_payload();

    // Sanity: all payloads must pass validation
    validate_canonical(&small).expect("small payload not canonical");
    validate_canonical(&medium).expect("medium payload not canonical");
    validate_canonical(&large).expect("large payload not canonical");
    validate_canonical(&very_large).expect("very_large payload not canonical");

    let mut group = c.benchmark_group("validate_canonical");

    group.bench_function(&format!("small_{}B", small.len()), |b| {
        b.iter(|| validate_canonical(black_box(&small)))
    });

    group.bench_function(&format!("medium_{}B", medium.len()), |b| {
        b.iter(|| validate_canonical(black_box(&medium)))
    });

    group.bench_function(&format!("large_{}B", large.len()), |b| {
        b.iter(|| validate_canonical(black_box(&large)))
    });

    group.bench_function(&format!("very_large_{}B", very_large.len()), |b| {
        b.iter(|| validate_canonical(black_box(&very_large)))
    });

    group.finish();
}

criterion_group!(benches, bench_validate_canonical);
criterion_main!(benches);
