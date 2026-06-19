// =============================================================================
// File: services/legal/revoke.rs — F2 P8.c soft-delete + audit emit for RODO PDFs
// =============================================================================
//
// Two responsibilities:
//   1. Stamp `legal_documents.revoked_at` for `(doc_id, org_id)` via the repo,
//   2. Emit a B-class `legal.revoke` audit row with the same Merkle chain
//      handling as `legal.generate` (see `rodo_generator::audit_emit_generate`).
//
// Async wrapper offloads the SQLite work to a blocking pool so a dashboard
// handler awaiting this does not stall a Tokio worker.

use rusqlite::{params, Connection};

use crate::audit::chain::{compute_chain_for_insert, AuditRowHashInput};
use crate::db::legal_documents::{get_by_id, revoke as repo_revoke, RevokeOutcome};
use crate::db::DbPool;

#[derive(Debug, thiserror::Error)]
pub enum RevokeError {
    /// `(doc_id, org_id)` does not resolve to a row. Collapsed shape — a caller
    /// from org A passing the id of an org B row gets the same error as a
    /// genuinely missing id (org isolation invariant).
    #[error("document not found")]
    NotFound,
    /// Caller is not a member of the target org. Distinct from `NotFound` only
    /// at the call site — the dispatch layer maps both to the same RPC error
    /// code so probing cannot tell them apart.
    #[error("user not a member of organization")]
    UserNotMember,
    /// The row already carries a `revoked_at` timestamp. Surfaced as a typed
    /// error so the caller can pick its HTTP/RPC mapping (409 vs 200), but no
    /// audit row is emitted at this layer — the original revoke already has
    /// its `legal.revoke` row in the chain.
    #[error("document already revoked")]
    AlreadyRevoked,
    #[error("database error")]
    Db(#[from] rusqlite::Error),
}

/// Soft-delete + audit emit. The membership check, `revoke()` SQL UPDATE, and
/// audit insert run under a single DB lock acquired by the caller (sync entry
/// point) — keeps the (revoke, audit) pair linearizable and ensures the audit
/// chain stays consistent.
pub fn revoke(
    conn: &Connection,
    org_id: &str,
    doc_id: &str,
    actor_user_id: &str,
    now_ms: i64,
) -> Result<(), RevokeError> {
    if !is_member(conn, org_id, actor_user_id)? {
        tracing::warn!(
            target: "tentaflow::legal::revoke",
            org_id = %org_id,
            user_id = %actor_user_id,
            doc_id = %doc_id,
            "revoke denied: user not member of org"
        );
        return Err(RevokeError::UserNotMember);
    }

    // Existence + tenant-scope check. `get_by_id` already filters by org_id so
    // a cross-tenant probe returns `Ok(None)` indistinguishably from a real
    // miss; we collapse both onto `NotFound`.
    let exists = get_by_id(conn, doc_id, org_id)
        .map_err(|e| anyhow_to_db(e))?
        .is_some();
    if !exists {
        return Err(RevokeError::NotFound);
    }

    // Idempotent UPDATE: the SQL guard `revoked_at IS NULL` ensures a second
    // revoke of the same row touches zero rows. We emit the audit chain entry
    // only on the freshly-revoked transition so a duplicate caller cannot
    // pollute the audit log with redundant rows (or overwrite the original
    // `revoked_at` timestamp).
    match repo_revoke(conn, doc_id, org_id, now_ms).map_err(anyhow_to_db)? {
        RevokeOutcome::FreshlyRevoked => {
            let _ = audit_emit_revoke(conn, org_id, actor_user_id, doc_id, now_ms);
            Ok(())
        }
        RevokeOutcome::AlreadyRevoked => Err(RevokeError::AlreadyRevoked),
    }
}

/// Async wrapper for use from Tokio handlers. Mirrors
/// `rodo_generator::generate_async` — same blocking-pool offload + poisoned-
/// mutex error shape so callers can treat the two surfaces uniformly.
pub async fn revoke_async(
    db: DbPool,
    org_id: String,
    doc_id: String,
    actor_user_id: String,
    now_ms: i64,
) -> Result<(), RevokeError> {
    tokio::task::spawn_blocking(move || {
        let conn = db.write().map_err(|_| {
            RevokeError::Db(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some("db pool mutex poisoned".to_string()),
            ))
        })?;
        revoke(&conn, &org_id, &doc_id, &actor_user_id, now_ms)
    })
    .await
    .map_err(|join_err| {
        RevokeError::Db(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some(format!("blocking task join: {join_err}")),
        ))
    })?
}

fn is_member(conn: &Connection, org_id: &str, user_id: &str) -> Result<bool, RevokeError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM org_memberships WHERE org_id = ?1 AND user_id = ?2",
        params![org_id, user_id],
        |row| row.get(0),
    )?;
    Ok(n > 0)
}

fn anyhow_to_db(e: anyhow::Error) -> RevokeError {
    if let Some(rs) = e.downcast_ref::<rusqlite::Error>() {
        return RevokeError::Db(rusqlite::Error::SqliteFailure(
            match rs {
                rusqlite::Error::SqliteFailure(code, _) => *code,
                _ => rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            },
            Some(rs.to_string()),
        ));
    }
    RevokeError::Db(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
        Some(e.to_string()),
    ))
}

fn audit_emit_revoke(
    conn: &Connection,
    org_id: &str,
    user_id: &str,
    doc_id: &str,
    now_ms: i64,
) -> Result<(), rusqlite::Error> {
    let details = serde_json::json!({
        "doc_id": doc_id,
        "user_id": user_id,
        "revoked_at_ms": now_ms,
    })
    .to_string();

    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let action = "legal.revoke";
    let resource_type = Some("legal_document");
    let resource_id = Some(doc_id);
    let result = Some("ok");
    let severity = Some("info");
    let risk_class = "B";
    let hash_input = AuditRowHashInput {
        user_id: None,
        addon_id: None,
        instance_id: None,
        action,
        resource: None,
        resource_type,
        resource_id,
        result,
        error_message: None,
        details: Some(details.as_str()),
        ip_address: None,
        node_id: None,
        severity,
        risk_class,
        related_claim_id: None,
        request_id: None,
        timestamp: &timestamp,
    };
    let (prev_hash, hash) = compute_chain_for_insert(conn, &hash_input)?;
    conn.execute(
        "INSERT INTO audit_log \
            (timestamp, action, resource_type, resource_id, result, \
             severity, risk_class, details, org_id, prev_hash, hash) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            timestamp,
            action,
            resource_type,
            resource_id,
            result,
            severity,
            risk_class,
            details,
            org_id,
            prev_hash,
            hash,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::legal_documents::{insert as insert_doc, NewLegalDocument};
    use crate::services::legal::RodoVariant;

    const ORG: &str = "11111111-1111-4111-8111-111111111111";
    const USER: &str = "u-test";

    fn full_hash() -> String {
        blake3::hash(b"x").to_hex().to_string()
    }

    fn open_db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).expect("run migrations");
        // Seed a dedicated UUIDv4 org the legal subsystem accepts. Renaming the
        // migration-seeded `org-default` PK in place would trip the 24 child-table
        // FKs that reference organizations(org_id).
        conn.execute(
            "INSERT INTO organizations (org_id, name, slug, contact_email, dpo_contact, retention_policy_json, status, created_at) \
             VALUES (?1, ?1, 'test-org', 'office@example.test', NULL, NULL, 'active', '2026-01-01T00:00:00Z')",
            params![ORG],
        )
        .unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO org_memberships \
                (org_id, user_id, role_id, granted_at, granted_by) \
             VALUES (?1, ?2, 'role-org-admin', '2026-01-01T00:00:00Z', 'system')",
            params![ORG, USER],
        )
        .unwrap();
        conn
    }

    fn seed_doc(conn: &Connection) -> String {
        insert_doc(
            conn,
            &NewLegalDocument {
                org_id: ORG.into(),
                variant: RodoVariant::Standard,
                generated_at: 1,
                generated_by_user_id: USER.into(),
                content_hash: full_hash(),
                pdf_path: "/tmp/x.pdf".into(),
                signed_url_ref: None,
            },
        )
        .unwrap()
    }

    #[test]
    fn revoke_happy_path() {
        let conn = open_db();
        let id = seed_doc(&conn);
        revoke(&conn, ORG, &id, USER, 42).expect("revoke");
        let row = get_by_id(&conn, &id, ORG).unwrap().unwrap();
        assert_eq!(row.revoked_at, Some(42));

        // Audit row emitted with the right action + risk class.
        let action: String = conn
            .query_row(
                "SELECT action FROM audit_log WHERE resource_id = ?1 ORDER BY id DESC LIMIT 1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(action, "legal.revoke");
    }

    #[test]
    fn revoke_rejects_missing_doc_as_not_found() {
        let conn = open_db();
        let ghost = "44444444-4444-4444-8444-444444444444";
        let err = revoke(&conn, ORG, ghost, USER, 1).unwrap_err();
        assert!(matches!(err, RevokeError::NotFound));
    }

    #[test]
    fn revoke_rejects_non_member() {
        let conn = open_db();
        let id = seed_doc(&conn);
        let err = revoke(&conn, ORG, &id, "u-ghost", 1).unwrap_err();
        assert!(matches!(err, RevokeError::UserNotMember));
    }

    #[test]
    fn revoke_already_revoked_returns_error_no_double_audit() {
        let conn = open_db();
        let id = seed_doc(&conn);
        revoke(&conn, ORG, &id, USER, 42).expect("first revoke");

        // Second revoke surfaces AlreadyRevoked and must NOT emit an audit row.
        let audit_count_before: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'legal.revoke' AND resource_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(audit_count_before, 1, "first revoke emits exactly one row");

        let err = revoke(&conn, ORG, &id, USER, 999).unwrap_err();
        assert!(matches!(err, RevokeError::AlreadyRevoked));

        let audit_count_after: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'legal.revoke' AND resource_id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            audit_count_after, 1,
            "second revoke must not emit a duplicate audit row"
        );

        // Original revoked_at timestamp preserved — the second call did not
        // overwrite the row.
        let row = get_by_id(&conn, &id, ORG).unwrap().unwrap();
        assert_eq!(row.revoked_at, Some(42));
    }
}
