// =============================================================================
// Plik: tests/sdk_boilerplate.rs
// Opis: Testy integracyjne SDK boilerplate F1a (M0.W2) — AbiError, sdk_version
//       compatibility, payload limits, audit_log risk_class w DB.
// =============================================================================

use rusqlite::Connection;
use tentaflow_core::addon::errors::AbiError;
use tentaflow_core::addon::host_functions::abi_helpers::{
    enforce_payload_size, PayloadKind,
};
use tentaflow_core::addon::sdk_version::{check_compatibility, SdkVersionError, CORE_SDK_VERSION};
use tentaflow_core::audit::RiskClass;

// =============================================================================
// AbiError — 24 kody
// =============================================================================

#[test]
fn abi_error_codes_unique_and_match_spec() {
    let pairs = [
        (AbiError::Ok, 0),
        (AbiError::Permission, 1),
        (AbiError::NotFound, 2),
        (AbiError::NoAvailableTarget, 3),
        (AbiError::Timeout, 4),
        (AbiError::Operation, 5),
        (AbiError::OutputBufferTooSmall, 6),
        (AbiError::Conflict, 7),
        (AbiError::SqlSyntax, 8),
        (AbiError::SqlConstraint, 9),
        (AbiError::SqlNoResult, 10),
        (AbiError::QuotaExceeded, 11),
        (AbiError::CameraUnreachable, 12),
        (AbiError::CameraAuthFailed, 13),
        (AbiError::CameraVendorUnsupported, 14),
        (AbiError::StreamNotFound, 15),
        (AbiError::StreamClosed, 16),
        (AbiError::Backpressure, 17),
        (AbiError::RecordingNotFound, 18),
        (AbiError::RecordingPurged, 19),
        (AbiError::RecordingTimeOutOfRing, 20),
        (AbiError::PayloadTooLarge, 21),
        (AbiError::GateNotSatisfied, 22),
        (AbiError::FrameTokenInvalid, 23),
        (AbiError::FramePurged, 24),
    ];

    let mut seen = std::collections::HashSet::new();
    for (variant, expected_code) in pairs {
        assert_eq!(variant.as_i32(), expected_code, "Bad i32 for {:?}", variant);
        assert!(seen.insert(expected_code), "Duplicate code {}", expected_code);
    }
    assert_eq!(seen.len(), 25, "Brakuje wariantow AbiError");
}

#[test]
fn abi_error_descriptions_exist() {
    let variants = [
        AbiError::Ok, AbiError::Permission, AbiError::NotFound,
        AbiError::NoAvailableTarget, AbiError::Timeout, AbiError::Operation,
        AbiError::OutputBufferTooSmall, AbiError::Conflict, AbiError::SqlSyntax,
        AbiError::SqlConstraint, AbiError::SqlNoResult, AbiError::QuotaExceeded,
        AbiError::CameraUnreachable, AbiError::CameraAuthFailed,
        AbiError::CameraVendorUnsupported, AbiError::StreamNotFound,
        AbiError::StreamClosed, AbiError::Backpressure, AbiError::RecordingNotFound,
        AbiError::RecordingPurged, AbiError::RecordingTimeOutOfRing,
        AbiError::PayloadTooLarge, AbiError::GateNotSatisfied,
        AbiError::FrameTokenInvalid, AbiError::FramePurged,
    ];
    for v in variants {
        assert!(!v.description().is_empty(), "Empty description for {:?}", v);
    }
}

#[test]
fn abi_error_into_i32_conversion() {
    let v: i32 = AbiError::PayloadTooLarge.into();
    assert_eq!(v, 21);
    let v2: i32 = AbiError::Ok.into();
    assert_eq!(v2, 0);
}

// =============================================================================
// Payload size limits
// =============================================================================

#[test]
fn payload_size_limits_per_kind() {
    assert_eq!(PayloadKind::ServiceCall.max_bytes(), 8 * 1024 * 1024);
    assert_eq!(PayloadKind::SqlCombined.max_bytes(), 4 * 1024 * 1024);
    assert_eq!(PayloadKind::VectorItem.max_bytes(), 1024 * 1024);
    assert_eq!(PayloadKind::UiRender.max_bytes(), 2 * 1024 * 1024);
    assert_eq!(PayloadKind::Secret.max_bytes(), 64 * 1024);
}

#[test]
fn enforce_payload_size_ok_and_err() {
    assert!(enforce_payload_size(1024, PayloadKind::ServiceCall).is_ok());
    assert!(enforce_payload_size(8 * 1024 * 1024, PayloadKind::ServiceCall).is_ok());

    let err = enforce_payload_size(9 * 1024 * 1024, PayloadKind::ServiceCall).unwrap_err();
    assert_eq!(err, AbiError::PayloadTooLarge);

    let err2 = enforce_payload_size(70_000, PayloadKind::Secret).unwrap_err();
    assert_eq!(err2, AbiError::PayloadTooLarge);
}

// =============================================================================
// out_cap retry pattern — test z wasmtime Memory
// =============================================================================

#[cfg(not(any(target_os = "ios", target_os = "android")))]
#[test]
fn out_cap_retry_pattern() {
    use tentaflow_core::addon::host_functions::abi_helpers::write_output_with_retry_semantics;
    use wasmtime::{Engine, Memory, MemoryType, Store};

    let engine = Engine::default();
    let mut store: Store<()> = Store::new(&engine, ());
    let mem = Memory::new(&mut store, MemoryType::new(1, None)).expect("memory");

    // Bufor wystarczy — wpisz 5 bajtow, out_cap=10, out_ptr=0, out_len_ptr=100.
    let data_small = b"hello";
    let rc = write_output_with_retry_semantics(&mem, &mut store, data_small, 0, 10, 100);
    assert_eq!(rc, AbiError::Ok.as_i32());
    let mut buf = [0u8; 5];
    mem.read(&store, 0, &mut buf).unwrap();
    assert_eq!(&buf, b"hello");
    let mut len_buf = [0u8; 4];
    mem.read(&store, 100, &mut len_buf).unwrap();
    assert_eq!(u32::from_le_bytes(len_buf), 5);

    // Bufor za maly — out_cap=3, dane=10 bajtow. Powinien zwrocic
    // OutputBufferTooSmall i zapisac wymagany rozmiar.
    let data_big = b"0123456789";
    let rc2 = write_output_with_retry_semantics(&mem, &mut store, data_big, 200, 3, 300);
    assert_eq!(rc2, AbiError::OutputBufferTooSmall.as_i32());
    let mut len_buf2 = [0u8; 4];
    mem.read(&store, 300, &mut len_buf2).unwrap();
    assert_eq!(u32::from_le_bytes(len_buf2), 10);
    // Bufor docelowy NIE byl modyfikowany (pozostal zerowy).
    let mut target = [0u8; 10];
    mem.read(&store, 200, &mut target).unwrap();
    assert_eq!(target, [0u8; 10]);
}

#[cfg(not(any(target_os = "ios", target_os = "android")))]
#[test]
fn test_write_overflow_returns_err() {
    // Regresja: na 32-bit hostach `start + actual_len` moglo wrap-around
    // gdy guest podal i32::MAX jako ptr. checked_add musi zwracac Operation.
    use tentaflow_core::addon::host_functions::abi_helpers::write_output_with_retry_semantics;
    use wasmtime::{Engine, Memory, MemoryType, Store};

    let engine = Engine::default();
    let mut store: Store<()> = Store::new(&engine, ());
    let mem = Memory::new(&mut store, MemoryType::new(1, None)).expect("memory");

    let data = vec![0xAAu8; 100];
    // out_ptr = i32::MAX, out_cap = 100 (≥ data.len()) — wchodzi w gałąź
    // write i tam checked_add musi zlapac overflow / out-of-bounds.
    let rc = write_output_with_retry_semantics(&mem, &mut store, &data, i32::MAX, 100, 0);
    assert_eq!(rc, AbiError::Operation.as_i32());
}

// =============================================================================
// SDK version compatibility
// =============================================================================

#[test]
fn sdk_version_check_none_ok() {
    assert!(check_compatibility(None).is_ok());
}

#[test]
fn sdk_version_check_exact_match() {
    assert!(check_compatibility(Some(CORE_SDK_VERSION)).is_ok());
}

#[test]
fn sdk_version_check_range_ok() {
    assert!(check_compatibility(Some(">=0.1.0, <1.0")).is_ok());
    assert!(check_compatibility(Some("^0.2")).is_ok());
}

#[test]
fn test_sdk_version_mismatch_returns_incompatible_variant() {
    let err = check_compatibility(Some(">=99.0")).unwrap_err();
    assert!(matches!(err, SdkVersionError::Incompatible { .. }));
}

#[test]
fn test_sdk_version_invalid_semver_returns_invalid_semver_variant() {
    let err = check_compatibility(Some("definitely not semver")).unwrap_err();
    assert!(matches!(err, SdkVersionError::InvalidSemver(_)));
}

// =============================================================================
// audit_log + risk_class — bezposredni SQL test (in-memory SQLite z pelnym
// schematem audit_log po migracji v7)
// =============================================================================

/// Buduje schemat tabeli audit_log identyczny z produkcyjnym po migracji 7.
fn audit_log_test_schema(conn: &Connection) {
    conn.execute_batch(
        r#"
        CREATE TABLE audit_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL DEFAULT (datetime('now')),
            user_id INTEGER,
            addon_id TEXT,
            action TEXT NOT NULL,
            resource TEXT,
            details TEXT,
            ip_address TEXT,
            node_id TEXT,
            instance_id TEXT,
            resource_type TEXT,
            resource_id TEXT,
            result TEXT,
            error_message TEXT,
            action_hash INTEGER,
            severity TEXT NOT NULL DEFAULT 'info',
            risk_class TEXT NOT NULL DEFAULT 'unclassified',
            related_claim_id TEXT,
            request_id TEXT
        );
        CREATE INDEX idx_audit_risk_class ON audit_log(risk_class) WHERE risk_class IN ('B','C');
        CREATE INDEX idx_audit_claim ON audit_log(related_claim_id) WHERE related_claim_id IS NOT NULL;
        CREATE INDEX idx_audit_request_id ON audit_log(request_id);
        "#,
    )
    .unwrap();
}

#[test]
fn audit_log_with_risk_class_c_inserted() {
    let conn = Connection::open_in_memory().unwrap();
    audit_log_test_schema(&conn);

    conn.execute(
        "INSERT INTO audit_log (addon_id, action, result, risk_class, related_claim_id, request_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            "tentavision",
            "service.call",
            "ok",
            RiskClass::C.as_db_str(),
            "claim-d4-historical-001",
            "req-abc-123"
        ],
    )
    .unwrap();

    let (rc, claim, req): (String, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT risk_class, related_claim_id, request_id FROM audit_log WHERE addon_id='tentavision'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(rc, "C");
    assert_eq!(claim.as_deref(), Some("claim-d4-historical-001"));
    assert_eq!(req.as_deref(), Some("req-abc-123"));

    // Indeks partial B/C zwraca wiersz.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE risk_class='C'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn audit_log_backward_compat_unclassified_default() {
    let conn = Connection::open_in_memory().unwrap();
    audit_log_test_schema(&conn);

    // Stary INSERT bez risk_class — kolumna ma DEFAULT 'unclassified'.
    conn.execute(
        "INSERT INTO audit_log (addon_id, action, result) VALUES (?1, ?2, ?3)",
        rusqlite::params!["teams-bot", "tool.call", "ok"],
    )
    .unwrap();

    let rc: String = conn
        .query_row(
            "SELECT risk_class FROM audit_log WHERE addon_id='teams-bot'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rc, "unclassified");
}

#[test]
fn risk_class_display_and_fromstr() {
    use std::str::FromStr;
    assert_eq!(format!("{}", RiskClass::B), "B");
    assert_eq!(RiskClass::from_str("A").unwrap(), RiskClass::A);
    assert_eq!(
        RiskClass::from_str("unclassified").unwrap(),
        RiskClass::Unclassified
    );
    assert!(RiskClass::from_str("Z").is_err());
}
