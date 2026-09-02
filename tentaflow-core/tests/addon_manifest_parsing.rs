// =============================================================================
// Plik: tests/addon_manifest_parsing.rs
// Opis: Testy parsera manifest.toml dla rozszerzen F1a — sekcje [storage],
//       [[alias]], [[gate]], [[vector_namespace]], [[flow_template]],
//       [[ui_component]], [gpu] oraz pole [addon].sdk_version.
//       Uruchomienie: cargo test --test addon_manifest_parsing
// =============================================================================

use tentaflow_core::addon::lifecycle::parse_manifest_toml;

// =============================================================================
// Minimalny manifest — backward compat
// =============================================================================

#[test]
fn test_parse_minimal_manifest_ok() {
    let toml = r#"
[addon]
id = "minimal"
name = "Minimal"
version = "0.1.0"
wasm_file = "addon.wasm"
"#;
    let m = parse_manifest_toml(toml).expect("minimal manifest must parse");
    assert_eq!(m.addon_id, "minimal");
    assert!(m.storage.is_none());
    assert!(m.aliases.is_empty());
    assert!(m.gates.is_empty());
    assert!(m.vector_namespaces.is_empty());
    assert!(m.flow_templates.is_empty());
    assert!(m.ui_components.is_empty());
    assert!(m.gpu.is_none());
    assert!(m.sdk_version.is_none());
}

// =============================================================================
// Pelny manifest TentaVision (kopia z notes/tentavision-plan.md §5)
// =============================================================================

const TENTAVISION_MANIFEST: &str = r##"
[addon]
id = "tentavision"
name = "TentaVision"
version = "0.1.0"
description = "Analiza obrazu z kamer"
category = "surveillance"
keywords = ["video","cctv"]
author = "TentaFlow"
license = "Commercial"
icon = "video"
runtime = "wasmtime"
platforms = ["linux"]
wasm_file = "tentavision.wasm"
sdk_version = ">=0.2.0"

[application]
entry_panel = "dashboard"
title = "TentaVision"
icon = "video"
sort_order = 100

[service]
enabled = true
tick_interval_ms = 250
tick_fuel_budget = 5000000
tick_timeout_ms = 1000

[visibility]
admin_only = false
show_in_catalog = true

[resources]
memory_mb = 256
fuel_limit = 10000000
storage_total_mb = 64
http_requests_per_minute = 60

[storage]
kv = true
sql = true
sql_backends = ["sqlite"]
sql_dialect = "ansi"
migrations_dir = "migrations"
encryption = "at-rest"

[[vector_namespace]]
name = "attributes"
dimensions = 768
distance = "cosine"
data_class = "B"

[[vector_namespace]]
name = "plates"
dimensions = 256
distance = "cosine"
data_class = "B"

[[vector_namespace]]
name = "faces"
dimensions = 512
distance = "cosine"
data_class = "C"
gate = "d4-historical"

[[vector_namespace]]
name = "persons"
dimensions = 512
distance = "cosine"
data_class = "C"
gate = "d4-historical"

[[alias]]
id = "tentavision-yolo"
display_name = "Detektor obiektow"
methods = ["detect", "track"]
suggested_default = "yolo11m-detector"

[[alias]]
id = "tentavision-ocr"
display_name = "OCR ADR"
methods = ["recognize", "recognize_cropped"]
suggested_default = "ppocrv5-ocr"

[[alias]]
id = "tentavision-action"
display_name = "Klasyfikator akcji"
methods = ["classify_window"]
suggested_default = ""

[[alias]]
id = "tentavision-vlm"
display_name = "VLM atrybuty"
methods = ["embed", "caption"]
suggested_default = "siglip2-vit-l14"

[[alias]]
id = "tentavision-face-embed"
display_name = "Face embedding"
methods = ["embed"]
suggested_default = ""
gate = "d4-historical"

[[alias]]
id = "tentavision-reid"
display_name = "Person re-id"
methods = ["embed", "match"]
suggested_default = ""
gate = "d4-historical"

[[permission]]
id = "service.call"
display_name = "service_call"
description = "..."
risk = "medium"

[[permission]]
id = "service.read"
display_name = "service.read"
description = "..."
risk = "low"

[[permission]]
id = "alias.read"
display_name = "alias.read"
description = "..."
risk = "medium"

[[permission]]
id = "camera.manage"
display_name = "camera.manage"
description = "..."
risk = "medium"

[[permission]]
id = "camera.read"
display_name = "camera.read"
description = "..."
risk = "low"

[[permission]]
id = "stream.subscribe"
display_name = "stream.subscribe"
description = "..."
risk = "medium"

[[permission]]
id = "sql.read"
display_name = "sql.read"
description = "..."
risk = "low"

[[permission]]
id = "sql.write"
display_name = "sql.write"
description = "..."
risk = "low"

[[permission]]
id = "vector.read"
display_name = "vector.read"
description = "..."
risk = "low"

[[permission]]
id = "vector.write"
display_name = "vector.write"
description = "..."
risk = "low"

[[permission]]
id = "recording.save"
display_name = "recording.save"
description = "..."
risk = "medium"

[[permission]]
id = "recording.read"
display_name = "recording.read"
description = "..."
risk = "medium"

[[permission]]
id = "evidence.sign"
display_name = "evidence.sign"
description = "..."
risk = "high"

[[permission]]
id = "events.publish"
display_name = "events.publish"
description = "..."
risk = "low"

[[permission]]
id = "events.subscribe"
display_name = "events.subscribe"
description = "..."
risk = "low"

[[permission]]
id = "flow.invoke"
display_name = "flow.invoke"
description = "..."
risk = "medium"

[[permission]]
id = "secrets.read"
display_name = "secrets.read"
description = "..."
risk = "high"

[[permission]]
id = "secrets.write"
display_name = "secrets.write"
description = "..."
risk = "medium"

[[permission]]
id = "audit.read"
display_name = "audit.read"
description = "..."
risk = "low"

[[permission]]
id = "audit.write_classC"
display_name = "audit.write_classC"
description = "..."
risk = "high"

[[permission]]
id = "claim.write"
display_name = "claim.write"
description = "..."
risk = "high"

[[permission]]
id = "ui.render"
display_name = "ui.render"
description = "..."
risk = "low"

[[flow_template]]
id = "tv-realtime-adr"
display_name = "Real-time ADR"
path = "flows/tv-realtime-adr.flow.json"
description = "real-time"

[[flow_template]]
id = "tv-alarm-enrich"
display_name = "Wzbogacenie alarmu"
path = "flows/tv-alarm-enrich.flow.json"
description = "enrich"

[[flow_template]]
id = "tv-evidence-export"
display_name = "Eksport dowodowy"
path = "flows/tv-evidence-export.flow.json"
description = "export"

[[gate]]
id = "d4-historical"
display_name = "Re-id historyczna"
required_claims = [
  { type = "approval", subject = "dpia", status = "signed" },
  { type = "grant", scope = "biometric:historical", valid = true, has_expiry = true },
]

[[gate]]
id = "d4-realtime"
display_name = "Re-id realtime"
required_claims = [
  { type = "approval", subject = "dpia", status = "signed" },
  { type = "approval", subject = "fria", status = "signed" },
  { type = "grant", scope = "biometric:realtime", valid = true, has_expiry = true },
  { type = "deployment_profile", oneof = ["lea", "critical_infra"] },
]

[[gate]]
id = "deployment_profile_lea_or_critical"
required_claims = [
  { type = "deployment_profile", oneof = ["lea", "critical_infra"] },
]

[publisher]
ed25519_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
label = "TentaVision Inc"

[[ui_component]]
id = "tv-video-grid"
display_name = "Video grid"
slot = "main"
src = "components/tv-video-grid.js"
signature = "ed25519:<base64-signature-placeholder>"
risk = "high"

[[ui_component]]
id = "tv-zone-editor"
display_name = "Zone editor"
slot = "main"
src = "components/tv-zone-editor.js"
signature = "ed25519:<base64-signature-placeholder>"
risk = "high"

[[ui_component]]
id = "tv-heatmap"
display_name = "Heatmap"
slot = "main"
src = "components/tv-heatmap.js"
signature = "ed25519:<base64-signature-placeholder>"
risk = "low"

[[ui_component]]
id = "tv-results-grid"
display_name = "Results grid"
slot = "main"
src = "components/tv-results-grid.js"
signature = "ed25519:<base64-signature-placeholder>"
risk = "medium"

[gpu]
recommended_vram_mb = 12000
notes = "Dla D2+D5 zalecane 24 GB"
"##;

#[test]
fn test_parse_full_tentavision_manifest_ok() {
    let m = parse_manifest_toml(TENTAVISION_MANIFEST).expect("TentaVision manifest must parse");

    assert_eq!(m.addon_id, "tentavision");
    assert_eq!(m.sdk_version.as_deref(), Some(">=0.2.0"));

    assert_eq!(
        m.declared_permissions.len(),
        22,
        "expected 22 permissions, got {}",
        m.declared_permissions.len()
    );
    assert_eq!(m.aliases.len(), 6);
    assert_eq!(m.vector_namespaces.len(), 4);
    assert_eq!(m.flow_templates.len(), 3);
    assert_eq!(m.ui_components.len(), 4);
    assert_eq!(m.gates.len(), 3);

    let storage = m.storage.expect("[storage] expected");
    assert!(storage.kv);
    assert!(storage.sql);
    assert_eq!(storage.sql_backends, vec!["sqlite".to_string()]);
    assert_eq!(storage.sql_dialect, "ansi");
    assert_eq!(storage.encryption, "at-rest");

    let gpu = m.gpu.expect("[gpu] expected");
    assert_eq!(gpu.recommended_vram_mb, Some(12000));

    // Aliasy z gate
    let face = m
        .aliases
        .iter()
        .find(|a| a.id == "tentavision-face-embed")
        .unwrap();
    assert_eq!(face.gate.as_deref(), Some("d4-historical"));

    // Vector namespace z gate
    let faces_ns = m
        .vector_namespaces
        .iter()
        .find(|v| v.name == "faces")
        .unwrap();
    assert_eq!(faces_ns.gate.as_deref(), Some("d4-historical"));
    assert_eq!(faces_ns.dimensions, 512);

    // Gate d4-realtime: 4 claimy
    let realtime = m.gates.iter().find(|g| g.id == "d4-realtime").unwrap();
    assert_eq!(realtime.required_claims.len(), 4);
}

// =============================================================================
// Storage
// =============================================================================

#[test]
fn test_parse_storage_kv_only_ok() {
    let toml = r#"
[addon]
id = "kv-only"
name = "kv"
version = "0.1.0"
wasm_file = "a.wasm"

[storage]
kv = true
sql = false
"#;
    let m = parse_manifest_toml(toml).unwrap();
    let s = m.storage.unwrap();
    assert!(s.kv);
    assert!(!s.sql);
    assert!(s.sql_backends.is_empty());
}

#[test]
fn test_parse_storage_sql_sqlite_ok() {
    let toml = r#"
[addon]
id = "sql-sqlite"
name = "sql"
version = "0.1.0"
wasm_file = "a.wasm"

[storage]
sql = true
sql_backends = ["sqlite"]
sql_dialect = "sqlite"
"#;
    let m = parse_manifest_toml(toml).unwrap();
    let s = m.storage.unwrap();
    assert!(s.sql);
    assert_eq!(s.sql_dialect, "sqlite");
}

#[test]
fn test_parse_storage_sql_both_backends_ok() {
    let toml = r#"
[addon]
id = "sql-both"
name = "sql"
version = "0.1.0"
wasm_file = "a.wasm"

[storage]
sql = true
sql_backends = ["sqlite", "postgres"]
sql_dialect = "ansi"
"#;
    let m = parse_manifest_toml(toml).unwrap();
    let s = m.storage.unwrap();
    assert_eq!(s.sql_backends.len(), 2);
}

#[test]
fn test_parse_storage_sql_invalid_dialect_err() {
    let toml = r#"
[addon]
id = "bad-dialect"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[storage]
sql = true
sql_backends = ["sqlite"]
sql_dialect = "postgresql"
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(
        err.contains("sql_dialect") && err.contains("postgresql"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_parse_storage_sql_true_requires_backends_err() {
    let toml = r#"
[addon]
id = "no-backends"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[storage]
sql = true
sql_backends = []
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("sql_backends"), "unexpected error: {err}");
}

// =============================================================================
// Aliasy
// =============================================================================

#[test]
fn test_parse_alias_duplicate_id_err() {
    let toml = r#"
[addon]
id = "dup-alias"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[[alias]]
id = "a"
display_name = "A"
methods = ["m1"]

[[alias]]
id = "a"
display_name = "A2"
methods = ["m2"]
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(
        err.contains("Duplicate alias id"),
        "unexpected error: {err}"
    );
}

// =============================================================================
// Vector namespace
// =============================================================================

#[test]
fn test_parse_vector_namespace_invalid_data_class_err() {
    let toml = r#"
[addon]
id = "bad-class"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[[vector_namespace]]
name = "ns1"
dimensions = 128
distance = "cosine"
data_class = "D"
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("data_class"), "unexpected error: {err}");
}

// =============================================================================
// UI component
// =============================================================================

#[test]
fn test_parse_ui_component_invalid_signature_err() {
    let toml = r#"
[addon]
id = "bad-sig"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[[ui_component]]
id = "u1"
display_name = "U1"
slot = "main"
src = "u1.js"
signature = "invalid"
risk = "low"
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("signature"), "unexpected error: {err}");
}

#[test]
fn test_parse_ui_component_invalid_risk_err() {
    let toml = r#"
[addon]
id = "bad-risk"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[[ui_component]]
id = "u1"
display_name = "U1"
slot = "main"
src = "u1.js"
signature = "ed25519:AAAA"
risk = "critical"
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("risk"), "unexpected error: {err}");
}

// =============================================================================
// Strict signature regex — Ed25519 = 64 bajty → base64 z paddingiem = 88 znakow.
// =============================================================================

#[test]
fn test_signature_short_base64_err() {
    let toml = r#"
[addon]
id = "bad-sig"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[[ui_component]]
id = "u1"
display_name = "U1"
slot = "main"
src = "u1.js"
signature = "ed25519:A=="
risk = "low"
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("signature"), "unexpected error: {err}");
}

#[test]
fn test_signature_wrong_padding_err() {
    // 86 znakow bez paddingu — wymagamy `==`.
    let sig_no_pad: String = "A".repeat(86);
    let toml = format!(
        r#"
[addon]
id = "bad-sig"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[[ui_component]]
id = "u1"
display_name = "U1"
slot = "main"
src = "u1.js"
signature = "ed25519:{sig_no_pad}"
risk = "low"
"#
    );
    let err = parse_manifest_toml(&toml).unwrap_err().to_string();
    assert!(err.contains("signature"), "unexpected error: {err}");
}

#[test]
fn test_signature_correct_88_chars_ok() {
    // Realistyczny ksztalt: 86 znakow base64 + "==" padding.
    let body: String = "A".repeat(86);
    let toml = format!(
        r#"
[addon]
id = "good-sig"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[publisher]
ed25519_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
label = "Test"

[[ui_component]]
id = "u1"
display_name = "U1"
slot = "main"
src = "u1.js"
signature = "ed25519:{body}=="
risk = "low"
"#
    );
    let m = parse_manifest_toml(&toml).expect("88-char ed25519 signature must parse");
    assert_eq!(m.ui_components.len(), 1);
}

#[test]
fn test_signature_placeholder_still_ok() {
    // Placeholder z draftu TentaVision (F1c packaging podmienia na realna sygnature).
    let toml = r#"
[addon]
id = "ph"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[publisher]
ed25519_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
label = "Test"

[[ui_component]]
id = "u1"
display_name = "U1"
slot = "main"
src = "u1.js"
signature = "ed25519:<base64-signature-placeholder>"
risk = "low"
"#;
    let m = parse_manifest_toml(toml).expect("placeholder signature must parse");
    assert_eq!(m.ui_components.len(), 1);
}

#[test]
fn test_ui_component_host_permissions_parsed() {
    let toml = r#"
[addon]
id = "with-perms"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[publisher]
ed25519_public_key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
label = "Test"

[[ui_component]]
id = "u-with-perms"
display_name = "U with perms"
slot = "main"
src = "u.js"
signature = "ed25519:<base64-signature-placeholder>"
risk = "low"
host_permissions = ["alias.read", "camera.read"]

[[ui_component]]
id = "u-presentational"
display_name = "U presentational"
slot = "main"
src = "u2.js"
signature = "ed25519:<base64-signature-placeholder>"
risk = "low"
"#;
    let m = parse_manifest_toml(toml).expect("host_permissions must parse");
    assert_eq!(m.ui_components.len(), 2);
    let with_perms = m
        .ui_components
        .iter()
        .find(|c| c.id == "u-with-perms")
        .unwrap();
    assert_eq!(
        with_perms.host_permissions,
        vec!["alias.read".to_string(), "camera.read".to_string()]
    );
    let presentational = m
        .ui_components
        .iter()
        .find(|c| c.id == "u-presentational")
        .unwrap();
    assert!(
        presentational.host_permissions.is_empty(),
        "default empty when omitted"
    );
}

// =============================================================================
// sdk_version
// =============================================================================

#[test]
fn test_parse_sdk_version_invalid_semver_err() {
    let toml = r#"
[addon]
id = "bad-sdk"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"
sdk_version = "not-a-version"
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("sdk_version"), "unexpected error: {err}");
}

// =============================================================================
// Gate z required_claims
// =============================================================================

#[test]
fn test_parse_gate_with_required_claims_ok() {
    let toml = r#"
[addon]
id = "gates"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[[gate]]
id = "g1"
display_name = "Gate 1"
required_claims = [
  { type = "approval", subject = "dpia", status = "signed" },
  { type = "deployment_profile", oneof = ["lea", "critical_infra"] },
]
"#;
    let m = parse_manifest_toml(toml).unwrap();
    assert_eq!(m.gates.len(), 1);
    let g = &m.gates[0];
    assert_eq!(g.id, "g1");
    assert_eq!(g.required_claims.len(), 2);
    assert_eq!(g.required_claims[0].claim_type, "approval");
    assert_eq!(g.required_claims[0].subject.as_deref(), Some("dpia"));
    assert_eq!(g.required_claims[1].oneof.len(), 2);
}

#[test]
fn test_parse_gate_invalid_claim_type_err() {
    let toml = r#"
[addon]
id = "bad-claim"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[[gate]]
id = "g1"
display_name = "Gate 1"
required_claims = [
  { type = "made_up_type" },
]
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("claim type"), "unexpected error: {err}");
}

// =============================================================================
// Backward compatibility — istniejace addony
// =============================================================================

#[test]
fn test_backward_compat_sdk_showcase() {
    let manifest =
        std::fs::read_to_string("addons/sdk-showcase/manifest.toml").expect("read manifest");
    let m = parse_manifest_toml(&manifest).expect("sdk-showcase manifest must still parse");
    assert_eq!(m.addon_id, "sdk-showcase");
    assert!(m.storage.is_some());
    assert!(m.aliases.is_empty());
    assert!(m.gpu.is_none());
}

#[test]
fn test_backward_compat_teams_bot() {
    let manifest =
        std::fs::read_to_string("addons-pro/teams-bot/manifest.toml").expect("read manifest");
    let m = parse_manifest_toml(&manifest).expect("teams-bot manifest must still parse");
    assert_eq!(m.addon_id, "teams-bot");
    assert!(m.storage.is_none());
    // teams-bot declares its alias set at M1.W5 — chunk B introduces
    // `[[alias]]` blocks (teams-stt, teams-tts, teams-summary, teams-vision-face).
    // The assertion guards parser compatibility, not the manifest's contents.
    assert!(
        !m.aliases.is_empty(),
        "teams-bot must declare aliases (post-M1.W5)"
    );
}

#[test]
fn test_backward_compat_mcp() {
    let manifest = std::fs::read_to_string("addons/mcp/manifest.toml").expect("read manifest");
    let m = parse_manifest_toml(&manifest).expect("mcp manifest must still parse");
    assert_eq!(m.addon_id, "mcp");
    assert_eq!(m.tools.len(), 5);
    assert!(m.network_rules.iter().any(|r| r.host == "*" && r.required));
}

#[test]
fn test_backward_compat_ibm_mcp() {
    let manifest = std::fs::read_to_string("addons/ibm-mcp/manifest.toml").expect("read manifest");
    let m = parse_manifest_toml(&manifest).expect("ibm-mcp manifest must still parse");
    assert_eq!(m.addon_id, "ibm-mcp");
    assert_eq!(m.tools.len(), 5);
    assert!(m.tools.iter().all(|t| t.name.starts_with("ibm_mcp_")));
    assert!(m
        .network_rules
        .iter()
        .any(|r| r.host == "*.ibm.com" && r.required));
}

// =============================================================================
// Duplikaty pozostalych kolekcji
// =============================================================================

#[test]
fn test_parse_vector_namespace_duplicate_name_err() {
    let toml = r#"
[addon]
id = "dup-ns"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[[vector_namespace]]
name = "x"
dimensions = 128
distance = "cosine"
data_class = "A"

[[vector_namespace]]
name = "x"
dimensions = 64
distance = "dot"
data_class = "B"
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(
        err.contains("Duplicate vector_namespace id"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_parse_flow_template_duplicate_id_err() {
    let toml = r#"
[addon]
id = "dup-flow"
name = "x"
version = "0.1.0"
wasm_file = "a.wasm"

[[flow_template]]
id = "f1"
display_name = "F1"
path = "f1.json"

[[flow_template]]
id = "f1"
display_name = "F1 again"
path = "f1b.json"
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(
        err.contains("Duplicate flow_template id"),
        "unexpected error: {err}"
    );
}

// =============================================================================
// F2 P3 — [runtime] section (per-addon concurrency / rate-limit overrides)
// =============================================================================

#[test]
fn manifest_runtime_section_accepts_max_concurrency() {
    let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
wasm_file = "a.wasm"

[runtime]
max_concurrency = 3
"#;
    let m = parse_manifest_toml(toml).expect("parse");
    let r = m.runtime_overrides.expect("[runtime] populated");
    assert_eq!(r.max_concurrency, Some(3));
    assert!(r.rate_limit_per_min.is_none());
}

#[test]
fn manifest_runtime_section_accepts_rate_limit_per_min() {
    let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
wasm_file = "a.wasm"

[runtime]
rate_limit_per_min = 500
"#;
    let m = parse_manifest_toml(toml).expect("parse");
    let r = m.runtime_overrides.expect("[runtime] populated");
    assert_eq!(r.rate_limit_per_min, Some(500));
    assert!(r.max_concurrency.is_none());
}

#[test]
fn manifest_runtime_section_absent_defaults_to_none() {
    let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
wasm_file = "a.wasm"
"#;
    let m = parse_manifest_toml(toml).expect("parse");
    assert!(m.runtime_overrides.is_none());
}

#[test]
fn manifest_runtime_section_zero_means_no_override() {
    // `0` is the documented "remove the override" sentinel — addons that
    // want the system default re-anchor by writing `0`, not by deleting
    // the section.
    let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
wasm_file = "a.wasm"

[runtime]
max_concurrency = 0
rate_limit_per_min = 0
"#;
    let m = parse_manifest_toml(toml).expect("parse");
    let r = m.runtime_overrides.expect("[runtime] populated");
    assert!(r.max_concurrency.is_none());
    assert!(r.rate_limit_per_min.is_none());
}

#[test]
fn manifest_runtime_section_rejects_non_integer() {
    let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
wasm_file = "a.wasm"

[runtime]
max_concurrency = "lots"
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("max_concurrency"), "unexpected error: {err}");
}

#[test]
fn manifest_runtime_section_rejects_negative() {
    let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
wasm_file = "a.wasm"

[runtime]
rate_limit_per_min = -1
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(
        err.contains("rate_limit_per_min"),
        "unexpected error: {err}"
    );
}

// =============================================================================
// App-platform: [native] section, runtime = "native", permission defaults,
// [[uses_app]] / [[uses_service]] declarations.
// =============================================================================

const NATIVE_MANIFEST: &str = r#"
[addon]
id = "tentanas"
name = "TentaNas"
version = "1.4.0"
runtime = "native"

[application]
entry_panel = "main"
title = "TentaNas"
icon = "disk"
title_key = "tentanas.app.name"
description_key = "tentanas.app.desc"

[native]
singleton = true
routes = ["tentanas", "tentanas-live"]
db_file = "tentanas.db"
i18n_namespace = "tentanas"
background_on_disable = true
disable_semantics = "stop"

[[permission]]
id = "nas.read"
display_name = "View NAS"
description = "Browse disks, pools and shares."
display_name_key = "tentanas.perm.read.name"
description_key = "tentanas.perm.read.desc"
risk = "low"
default = "allow"

[[permission]]
id = "nas.admin"
display_name = "Administer NAS"
description = "Destructive operations."
risk = "critical"

[[uses_app]]
package_id = "ml-studio"
optional = true

[[uses_service]]
engine_id = "teams-bot"
"#;

#[test]
fn native_manifest_parses_with_platform_sections() {
    let m = parse_manifest_toml(NATIVE_MANIFEST).expect("native manifest must parse");
    assert!(m.is_native());
    assert_eq!(m.wasm_file, "", "native package must not fabricate a wasm path");
    let native = m.native.expect("[native] section");
    assert!(native.singleton);
    assert_eq!(native.routes, vec!["tentanas", "tentanas-live"]);
    assert_eq!(native.db_file.as_deref(), Some("tentanas.db"));
    assert_eq!(native.i18n_namespace.as_deref(), Some("tentanas"));
    assert!(native.background_on_disable);
    assert_eq!(native.disable_semantics, "stop");

    let app = m.application.expect("[application] section");
    assert_eq!(app.title_key.as_deref(), Some("tentanas.app.name"));

    assert_eq!(m.declared_permissions.len(), 2);
    let read = &m.declared_permissions[0];
    assert_eq!(read.default_grant, "allow");
    assert_eq!(read.display_name_key.as_deref(), Some("tentanas.perm.read.name"));
    let admin = &m.declared_permissions[1];
    assert_eq!(admin.default_grant, "deny", "default grant is deny-by-default");
    assert!(admin.display_name_key.is_none());

    assert_eq!(m.uses_apps.len(), 1);
    assert_eq!(m.uses_apps[0].package_id, "ml-studio");
    assert!(m.uses_apps[0].optional);
    assert_eq!(m.uses_services.len(), 1);
    assert_eq!(m.uses_services[0].engine_id, "teams-bot");
    assert!(!m.uses_services[0].optional);
}

#[test]
fn native_runtime_requires_native_section() {
    let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
runtime = "native"

[application]
entry_panel = "main"
title = "X"
icon = "apps"
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("[native]"), "unexpected error: {err}");
}

#[test]
fn native_section_requires_native_runtime() {
    let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
runtime = "wasmtime"
wasm_file = "a.wasm"

[native]
routes = ["x"]
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("runtime = \"native\""), "unexpected error: {err}");
}

#[test]
fn native_package_rejects_wasm_file() {
    let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
runtime = "native"
wasm_file = "a.wasm"

[application]
entry_panel = "main"
title = "X"
icon = "apps"

[native]
routes = ["x"]
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("wasm_file"), "unexpected error: {err}");
}

#[test]
fn native_package_requires_application_section() {
    let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
runtime = "native"

[native]
routes = ["x"]
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("[application]"), "unexpected error: {err}");
}

#[test]
fn native_section_validates_routes_and_semantics() {
    let bad_route = NATIVE_MANIFEST.replace(
        "routes = [\"tentanas\", \"tentanas-live\"]",
        "routes = [\"Bad Route\"]",
    );
    let err = parse_manifest_toml(&bad_route).unwrap_err().to_string();
    assert!(err.contains("native.routes"), "unexpected error: {err}");

    let bad_sem = NATIVE_MANIFEST.replace(
        "disable_semantics = \"stop\"",
        "disable_semantics = \"pause\"",
    );
    let err = parse_manifest_toml(&bad_sem).unwrap_err().to_string();
    assert!(err.contains("disable_semantics"), "unexpected error: {err}");

    let bad_db = NATIVE_MANIFEST.replace(
        "db_file = \"tentanas.db\"",
        "db_file = \"../escape.db\"",
    );
    let err = parse_manifest_toml(&bad_db).unwrap_err().to_string();
    assert!(err.contains("db_file"), "unexpected error: {err}");
}

#[test]
fn permission_default_grant_is_validated() {
    let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
wasm_file = "a.wasm"

[[permission]]
id = "p"
display_name = "P"
description = "d"
default = "maybe"
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("'allow' or 'deny'"), "unexpected error: {err}");
}

#[test]
fn uses_app_rejects_self_dependency() {
    let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
wasm_file = "a.wasm"

[[uses_app]]
package_id = "x"
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("itself"), "unexpected error: {err}");
}

#[test]
fn unknown_runtime_lists_native_in_error() {
    let toml = r#"
[addon]
id = "x"
name = "X"
version = "0.1.0"
runtime = "jvm"
wasm_file = "a.wasm"
"#;
    let err = parse_manifest_toml(toml).unwrap_err().to_string();
    assert!(err.contains("native"), "unexpected error: {err}");
}
