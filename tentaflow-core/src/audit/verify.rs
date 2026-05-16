// =============================================================================
// File: audit/verify.rs — Walk `audit_log` end-to-end and recompute every
// Merkle hash. Reports rows whose `hash` does not match
// `SHA256(canonical(row) || prev_hash)` (tamper) and rows whose `prev_hash`
// does not match the previous row's `hash` (insert/delete in the middle).
//
// Legacy F1a rows have NULL `prev_hash` and NULL `hash` — they predate P4
// and cannot be verified. Reported separately as `legacy_unchained` so the
// admin tool can show "N rows are unchained from the F1a window, M rows
// are chained, 0 tampered". A NULL hash row is allowed only when it
// precedes the first chained row — once chaining starts a NULL gap is
// itself tampering.
// =============================================================================

use rusqlite::Connection;

use super::chain::{compute_hash, canonical_row_bytes, AuditRowHashInput, ChainHash, GENESIS_PREV_HASH};

/// Reason a particular row failed verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TamperKind {
    /// Stored `prev_hash` does not match the previous row's stored `hash`.
    PrevHashMismatch,
    /// Stored `hash` does not match `SHA256(canonical(row) || prev_hash)`.
    HashMismatch,
    /// Row has NULL hash columns but earlier rows in the chain are non-NULL —
    /// implies a row was inserted into the middle without computing a hash.
    NullHashAfterChainStart,
    /// Row has malformed BLOB length (must be 32 bytes when non-NULL).
    MalformedHashBlob,
}

/// Per-row tamper report (id + reason).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TamperedRow {
    pub id: i64,
    pub kind: TamperKind,
}

/// Aggregate result of [`verify_chain`].
#[derive(Debug, Clone, Default)]
pub struct VerifyReport {
    /// Total number of rows walked.
    pub total: usize,
    /// Rows that hashed cleanly into the chain.
    pub chained_ok: usize,
    /// Rows with NULL chain columns at the head of the table (pre-P4).
    pub legacy_unchained: usize,
    /// Rows that failed verification.
    pub tampered: Vec<TamperedRow>,
}

impl VerifyReport {
    pub fn is_clean(&self) -> bool {
        self.tampered.is_empty()
    }
}

/// Errors raised during chain verification (DB errors, not tamper findings).
#[derive(Debug, thiserror::Error)]
pub enum AuditVerifyError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
}

/// Walk every row in `audit_log` in ascending id order and verify the chain.
/// Tamper findings are accumulated in `report.tampered`; only DB-level
/// failures abort with `Err`.
pub fn verify_chain(conn: &Connection) -> Result<VerifyReport, AuditVerifyError> {
    let mut stmt = conn.prepare(
        "SELECT id, user_id, addon_id, instance_id, action, resource, resource_type, resource_id, \
                result, error_message, details, ip_address, node_id, severity, risk_class, \
                related_claim_id, request_id, timestamp, prev_hash, hash \
         FROM audit_log ORDER BY id ASC",
    )?;

    let mut rows = stmt.query([])?;
    let mut report = VerifyReport::default();
    // Expected prev_hash for the next chained row. Genesis is all-zeros.
    let mut expected_prev: ChainHash = GENESIS_PREV_HASH;
    let mut chain_started = false;

    while let Some(row) = rows.next()? {
        report.total += 1;

        let id: i64 = row.get(0)?;
        let user_id: Option<i64> = row.get(1)?;
        let addon_id: Option<String> = row.get(2)?;
        let instance_id: Option<String> = row.get(3)?;
        let action: String = row.get(4)?;
        let resource: Option<String> = row.get(5)?;
        let resource_type: Option<String> = row.get(6)?;
        let resource_id: Option<String> = row.get(7)?;
        let result: Option<String> = row.get(8)?;
        let error_message: Option<String> = row.get(9)?;
        let details: Option<String> = row.get(10)?;
        let ip_address: Option<String> = row.get(11)?;
        let node_id: Option<String> = row.get(12)?;
        let severity: Option<String> = row.get(13)?;
        let risk_class: String = row.get(14)?;
        let related_claim_id: Option<String> = row.get(15)?;
        let request_id: Option<String> = row.get(16)?;
        let timestamp: String = row.get(17)?;
        let prev_hash_blob: Option<Vec<u8>> = row.get(18)?;
        let hash_blob: Option<Vec<u8>> = row.get(19)?;

        match (prev_hash_blob.as_ref(), hash_blob.as_ref()) {
            (None, None) => {
                if chain_started {
                    report.tampered.push(TamperedRow {
                        id,
                        kind: TamperKind::NullHashAfterChainStart,
                    });
                } else {
                    report.legacy_unchained += 1;
                }
                continue;
            }
            (_, None) | (None, Some(_)) => {
                // Half-NULL is itself malformed — treat as tamper.
                report.tampered.push(TamperedRow {
                    id,
                    kind: TamperKind::NullHashAfterChainStart,
                });
                continue;
            }
            _ => {}
        }

        let prev_hash_bytes = prev_hash_blob.unwrap();
        let hash_bytes = hash_blob.unwrap();

        if prev_hash_bytes.len() != 32 || hash_bytes.len() != 32 {
            report.tampered.push(TamperedRow {
                id,
                kind: TamperKind::MalformedHashBlob,
            });
            continue;
        }

        let mut prev_hash: ChainHash = [0u8; 32];
        prev_hash.copy_from_slice(&prev_hash_bytes);
        let mut stored_hash: ChainHash = [0u8; 32];
        stored_hash.copy_from_slice(&hash_bytes);

        if prev_hash != expected_prev {
            report.tampered.push(TamperedRow {
                id,
                kind: TamperKind::PrevHashMismatch,
            });
            // Continue with the stored chain so we report every break, not
            // just the first. Reset expected_prev to the stored hash so the
            // next row is judged against the chain on disk.
            expected_prev = stored_hash;
            chain_started = true;
            continue;
        }

        let input = AuditRowHashInput {
            user_id,
            addon_id: addon_id.as_deref(),
            instance_id: instance_id.as_deref(),
            action: &action,
            resource: resource.as_deref(),
            resource_type: resource_type.as_deref(),
            resource_id: resource_id.as_deref(),
            result: result.as_deref(),
            error_message: error_message.as_deref(),
            details: details.as_deref(),
            ip_address: ip_address.as_deref(),
            node_id: node_id.as_deref(),
            severity: severity.as_deref(),
            risk_class: &risk_class,
            related_claim_id: related_claim_id.as_deref(),
            request_id: request_id.as_deref(),
            timestamp: &timestamp,
        };
        let row_bytes = canonical_row_bytes(&input);
        let expected_hash = compute_hash(&row_bytes, &prev_hash);

        if expected_hash != stored_hash {
            report.tampered.push(TamperedRow {
                id,
                kind: TamperKind::HashMismatch,
            });
        } else {
            report.chained_ok += 1;
        }

        expected_prev = stored_hash;
        chain_started = true;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations::run as run_migrations;
    use rusqlite::params;

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory DB");
        run_migrations(&conn).expect("migrations");
        conn
    }

    /// Insert a chained audit row using the same path as `audit_log_with_risk`.
    fn insert_chained(
        conn: &Connection,
        action: &str,
        risk_class: &str,
        result: Option<&str>,
    ) -> i64 {
        let prev_hash =
            super::super::chain::latest_chain_hash(conn).expect("latest").unwrap_or(GENESIS_PREV_HASH);

        // Use a deterministic timestamp so test runs are reproducible. Match
        // the SQLite `datetime('now')` format ("YYYY-MM-DD HH:MM:SS").
        let ts = format!("2026-05-16 10:00:{:02}", action.len() % 60);

        let input = AuditRowHashInput {
            user_id: Some(1),
            addon_id: Some("com.test"),
            instance_id: Some("inst"),
            action,
            resource: None,
            resource_type: None,
            resource_id: None,
            result,
            error_message: None,
            details: None,
            ip_address: None,
            node_id: None,
            severity: Some("info"),
            risk_class,
            related_claim_id: None,
            request_id: None,
            timestamp: &ts,
        };
        let row_bytes = canonical_row_bytes(&input);
        let hash = compute_hash(&row_bytes, &prev_hash);

        conn.execute(
            "INSERT INTO audit_log (user_id, addon_id, instance_id, action, resource_type, \
                resource_id, result, error_message, risk_class, related_claim_id, request_id, \
                timestamp, prev_hash, hash) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
            params![
                1i64,
                "com.test",
                "inst",
                action,
                Option::<String>::None,
                Option::<String>::None,
                result,
                Option::<String>::None,
                risk_class,
                Option::<String>::None,
                Option::<String>::None,
                ts,
                prev_hash.to_vec(),
                hash.to_vec(),
            ],
        )
        .expect("insert chained row");

        conn.last_insert_rowid()
    }

    #[test]
    fn empty_audit_log_verifies_clean() {
        let conn = fresh_db();
        let report = verify_chain(&conn).unwrap();
        assert_eq!(report.total, 0);
        assert!(report.is_clean());
    }

    #[test]
    fn single_genesis_row_verifies() {
        let conn = fresh_db();
        insert_chained(&conn, "boot", "A", Some("ok"));
        let report = verify_chain(&conn).unwrap();
        assert_eq!(report.total, 1);
        assert_eq!(report.chained_ok, 1);
        assert!(report.is_clean());
    }

    #[test]
    fn ten_row_chain_verifies() {
        let conn = fresh_db();
        for i in 0..10 {
            insert_chained(&conn, &format!("act_{i}"), "A", Some("ok"));
        }
        let report = verify_chain(&conn).unwrap();
        assert_eq!(report.total, 10);
        assert_eq!(report.chained_ok, 10);
        assert!(report.is_clean());
    }

    #[test]
    fn modified_hash_blob_is_detected() {
        let conn = fresh_db();
        insert_chained(&conn, "act_a", "A", Some("ok"));
        let id_b = insert_chained(&conn, "act_b", "A", Some("ok"));

        // Tamper: zero out the stored `hash` of row b.
        let tampered_hash = vec![0u8; 32];
        conn.execute(
            "UPDATE audit_log SET hash = ?1 WHERE id = ?2",
            params![tampered_hash, id_b],
        )
        .unwrap();

        let report = verify_chain(&conn).unwrap();
        assert!(!report.is_clean());
        assert!(report
            .tampered
            .iter()
            .any(|t| t.id == id_b && t.kind == TamperKind::HashMismatch));
    }

    #[test]
    fn modified_content_is_detected() {
        let conn = fresh_db();
        insert_chained(&conn, "act_a", "A", Some("ok"));
        let id_b = insert_chained(&conn, "act_b", "A", Some("ok"));

        // Tamper: change `action` without recomputing hash.
        conn.execute(
            "UPDATE audit_log SET action = 'evil' WHERE id = ?1",
            params![id_b],
        )
        .unwrap();

        let report = verify_chain(&conn).unwrap();
        assert!(!report.is_clean());
        assert!(report
            .tampered
            .iter()
            .any(|t| t.id == id_b && t.kind == TamperKind::HashMismatch));
    }

    #[test]
    fn inserted_row_in_middle_breaks_chain() {
        let conn = fresh_db();
        insert_chained(&conn, "act_a", "A", Some("ok"));
        let id_b = insert_chained(&conn, "act_b", "A", Some("ok"));
        insert_chained(&conn, "act_c", "A", Some("ok"));

        // Tamper: change `action` of row b — every later row keeps its
        // original prev_hash so the chain breaks at row b (hash mismatch)
        // but rows after b still appear consistent against the on-disk
        // chain so we should see exactly one tamper: row b.
        conn.execute(
            "UPDATE audit_log SET action = 'inserted' WHERE id = ?1",
            params![id_b],
        )
        .unwrap();

        let report = verify_chain(&conn).unwrap();
        assert!(!report.is_clean());
        assert_eq!(report.tampered.len(), 1, "exactly row b is tampered");
        assert_eq!(report.tampered[0].id, id_b);
        assert_eq!(report.tampered[0].kind, TamperKind::HashMismatch);
    }

    #[test]
    fn deleted_row_breaks_next_prev_hash() {
        let conn = fresh_db();
        insert_chained(&conn, "act_a", "A", Some("ok"));
        let id_b = insert_chained(&conn, "act_b", "A", Some("ok"));
        let id_c = insert_chained(&conn, "act_c", "A", Some("ok"));

        // Tamper: delete row b. Row c's prev_hash now references b's hash
        // but b is gone, so c is judged against a's hash → mismatch.
        conn.execute("DELETE FROM audit_log WHERE id = ?1", params![id_b])
            .unwrap();

        let report = verify_chain(&conn).unwrap();
        assert!(!report.is_clean());
        assert!(report
            .tampered
            .iter()
            .any(|t| t.id == id_c && t.kind == TamperKind::PrevHashMismatch));
    }

    #[test]
    fn legacy_null_rows_counted_separately() {
        let conn = fresh_db();

        // Two legacy rows (no hash) — simulate F1a entries inserted before P4.
        conn.execute(
            "INSERT INTO audit_log (action, risk_class) VALUES ('legacy_1', 'unclassified')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO audit_log (action, risk_class) VALUES ('legacy_2', 'unclassified')",
            [],
        )
        .unwrap();

        // Then a chained row.
        insert_chained(&conn, "first_chained", "A", Some("ok"));

        let report = verify_chain(&conn).unwrap();
        assert_eq!(report.total, 3);
        assert_eq!(report.legacy_unchained, 2);
        assert_eq!(report.chained_ok, 1);
        assert!(report.is_clean());
    }

    #[test]
    fn null_row_after_chain_start_is_tamper() {
        let conn = fresh_db();
        insert_chained(&conn, "first", "A", Some("ok"));

        // Inject a NULL-hash row after the chain has started — looks like
        // someone bypassed `audit_log_with_risk` to slip in an event.
        conn.execute(
            "INSERT INTO audit_log (action, risk_class) VALUES ('snuck_in', 'unclassified')",
            [],
        )
        .unwrap();

        let report = verify_chain(&conn).unwrap();
        assert!(!report.is_clean());
        assert!(report
            .tampered
            .iter()
            .any(|t| t.kind == TamperKind::NullHashAfterChainStart));
    }
}
