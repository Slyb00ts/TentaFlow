// =============================================================================
// Plik: db/legal_documents.rs
// Opis: F2 P8.a repo for the `legal_documents` table — RODO/GDPR PDF artifacts.
//       Every read / write filters by `org_id` so a caller can never touch
//       another tenant's documents (org isolation invariant). The variant
//       enum is owned by `services::legal::RodoVariant`.
// =============================================================================

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use crate::services::legal::RodoVariant;

/// Fields needed to create a new legal_documents row. The `id` is minted by
/// the repository (UUIDv4) so callers cannot accidentally collide or smuggle
/// a non-UUID value past the CHECK constraint.
#[derive(Debug, Clone)]
pub struct NewLegalDocument {
    pub org_id: String,
    pub variant: RodoVariant,
    pub generated_at: i64,
    pub generated_by_user_id: String,
    pub content_hash: String,
    pub pdf_path: String,
    pub signed_url_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegalDocument {
    pub id: String,
    pub org_id: String,
    pub variant: RodoVariant,
    pub generated_at: i64,
    pub generated_by_user_id: String,
    pub content_hash: String,
    pub pdf_path: String,
    pub signed_url_ref: Option<String>,
    pub revoked_at: Option<i64>,
}

fn row_to_doc(row: &rusqlite::Row<'_>) -> rusqlite::Result<LegalDocument> {
    let variant_str: String = row.get(2)?;
    let variant = RodoVariant::from_str(&variant_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            2,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown legal_documents.variant value: {variant_str}"),
            )),
        )
    })?;
    Ok(LegalDocument {
        id: row.get(0)?,
        org_id: row.get(1)?,
        variant,
        generated_at: row.get(3)?,
        generated_by_user_id: row.get(4)?,
        content_hash: row.get(5)?,
        pdf_path: row.get(6)?,
        signed_url_ref: row.get(7)?,
        revoked_at: row.get(8)?,
    })
}

const COLS: &str = "id, org_id, variant, generated_at, generated_by_user_id, \
                    content_hash, pdf_path, signed_url_ref, revoked_at";

/// Insert a new legal document. The row id is minted here (UUIDv4) and
/// returned to the caller; `revoked_at` is always NULL on insert. The CHECK
/// constraints on `id` and `content_hash` are enforced by SQLite — a malformed
/// hash surfaces as a constraint violation, not as silent corruption.
pub fn insert(conn: &Connection, doc: &NewLegalDocument) -> Result<String> {
    let id = Uuid::new_v4().to_string();
    let affected = conn.execute(
        "INSERT INTO legal_documents \
            (id, org_id, variant, generated_at, generated_by_user_id, \
             content_hash, pdf_path, signed_url_ref, revoked_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)",
        params![
            id,
            doc.org_id,
            doc.variant.as_str(),
            doc.generated_at,
            doc.generated_by_user_id,
            doc.content_hash,
            doc.pdf_path,
            doc.signed_url_ref,
        ],
    )?;
    if affected != 1 {
        return Err(anyhow!(
            "legal_documents insert affected {} rows (expected 1)",
            affected
        ));
    }
    Ok(id)
}

/// List documents for an org, ordered newest first. `include_revoked = false`
/// hides soft-deleted rows from the default dashboard view.
pub fn list_by_org(
    conn: &Connection,
    org_id: &str,
    include_revoked: bool,
) -> Result<Vec<LegalDocument>> {
    let sql = if include_revoked {
        format!(
            "SELECT {COLS} FROM legal_documents \
             WHERE org_id = ?1 \
             ORDER BY generated_at DESC"
        )
    } else {
        format!(
            "SELECT {COLS} FROM legal_documents \
             WHERE org_id = ?1 AND revoked_at IS NULL \
             ORDER BY generated_at DESC"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![org_id], row_to_doc)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Fetch a single document. Tenant-scoped on purpose: a caller from org A
/// passing the id of an org B row gets `Ok(None)`, same shape as a genuine
/// miss. No row leak across tenants.
pub fn get_by_id(conn: &Connection, doc_id: &str, org_id: &str) -> Result<Option<LegalDocument>> {
    let sql = format!(
        "SELECT {COLS} FROM legal_documents \
         WHERE id = ?1 AND org_id = ?2"
    );
    let row = conn
        .query_row(&sql, params![doc_id, org_id], row_to_doc)
        .optional()?;
    Ok(row)
}

/// Attach the signed-URL HMAC reference once it has been minted by the
/// recording_url tier. Returns `Err` if the row does not exist for this org.
pub fn set_signed_url_ref(conn: &Connection, doc_id: &str, org_id: &str, ref_: &str) -> Result<()> {
    let affected = conn.execute(
        "UPDATE legal_documents SET signed_url_ref = ?1 \
         WHERE id = ?2 AND org_id = ?3",
        params![ref_, doc_id, org_id],
    )?;
    if affected != 1 {
        return Err(anyhow!(
            "legal_documents set_signed_url_ref: no row for id={doc_id} org_id={org_id}"
        ));
    }
    Ok(())
}

/// Soft-delete by stamping `revoked_at`. Idempotent at the SQL level (a row
/// already revoked is overwritten with the new timestamp); the caller decides
/// whether to re-revoke or short-circuit.
/// Soft-delete the row. Idempotent at the SQL level: the `revoked_at IS NULL`
/// guard ensures a second revoke of the same `(doc_id, org_id)` updates zero
/// rows and returns `Ok(RevokeOutcome::AlreadyRevoked)` — the original
/// timestamp and audit trail stay untouched. Callers distinguish a genuinely
/// missing row via the pre-check (`get_by_id`).
pub fn revoke(conn: &Connection, doc_id: &str, org_id: &str, now_ms: i64) -> Result<RevokeOutcome> {
    let affected = conn.execute(
        "UPDATE legal_documents SET revoked_at = ?1 \
         WHERE id = ?2 AND org_id = ?3 AND revoked_at IS NULL",
        params![now_ms, doc_id, org_id],
    )?;
    match affected {
        0 => Ok(RevokeOutcome::AlreadyRevoked),
        1 => Ok(RevokeOutcome::FreshlyRevoked),
        n => Err(anyhow!(
            "legal_documents revoke: unexpected affected row count {n} for id={doc_id} org_id={org_id}"
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeOutcome {
    FreshlyRevoked,
    AlreadyRevoked,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn open_test_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).expect("run migrations");
        // The default org row is seeded by v32; verify it before the test
        // proceeds so a future schema change cannot silently break isolation
        // assertions below.
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM organizations WHERE org_id = 'org-default'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "org-default missing from v32 seed");
        conn
    }

    fn full_hash() -> String {
        blake3::hash(b"test").to_hex().to_string()
    }

    fn make_doc(org: &str, variant: RodoVariant, ts: i64) -> NewLegalDocument {
        NewLegalDocument {
            org_id: org.into(),
            variant,
            generated_at: ts,
            generated_by_user_id: "u-1".into(),
            content_hash: full_hash(),
            pdf_path: format!("/tmp/{org}-{ts}.pdf"),
            signed_url_ref: None,
        }
    }

    fn seed_org(conn: &Connection, id: &str, slug: &str) {
        conn.execute(
            "INSERT INTO organizations (org_id, name, slug, status, created_at) \
             VALUES (?1, ?2, ?3, 'active', '2026-01-01T00:00:00Z')",
            params![id, id, slug],
        )
        .unwrap();
        // Seed the `u-1` membership the composite FK on legal_documents
        // requires for every (org_id, generated_by_user_id) pair the tests use.
        seed_membership(conn, id, "u-1");
    }

    fn seed_membership(conn: &Connection, org_id: &str, user_id: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO org_memberships \
                (org_id, user_id, role_id, granted_at, granted_by) \
             VALUES (?1, ?2, 'role-org-admin', '2026-01-01T00:00:00Z', 'system')",
            params![org_id, user_id],
        )
        .unwrap();
    }

    #[test]
    fn insert_and_get_round_trip() {
        let conn = open_test_conn();
        seed_membership(&conn, "org-default", "u-1");
        let new = make_doc("org-default", RodoVariant::Standard, 1000);
        let id = insert(&conn, &new).unwrap();
        let got = get_by_id(&conn, &id, "org-default").unwrap().unwrap();
        assert_eq!(got.id, id);
        assert_eq!(got.org_id, new.org_id);
        assert_eq!(got.variant, new.variant);
        assert_eq!(got.generated_at, new.generated_at);
        assert_eq!(got.generated_by_user_id, new.generated_by_user_id);
        assert_eq!(got.content_hash, new.content_hash);
        assert_eq!(got.pdf_path, new.pdf_path);
        assert_eq!(got.signed_url_ref, None);
        assert_eq!(got.revoked_at, None);
        // UUIDv4 shape — 36 chars, dashes at the canonical offsets.
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[8], b'-');
        assert_eq!(id.as_bytes()[13], b'-');
        assert_eq!(id.as_bytes()[18], b'-');
        assert_eq!(id.as_bytes()[23], b'-');
    }

    #[test]
    fn insert_rejects_short_content_hash() {
        let conn = open_test_conn();
        seed_membership(&conn, "org-default", "u-1");
        let mut new = make_doc("org-default", RodoVariant::Short, 1);
        new.content_hash = "deadbeef".into();
        let err = insert(&conn, &new).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("check"),
            "expected CHECK violation, got: {err}"
        );
    }

    #[test]
    fn insert_rejects_unknown_membership() {
        let conn = open_test_conn();
        // No membership seeded for u-ghost — composite FK must reject.
        let mut new = make_doc("org-default", RodoVariant::Short, 1);
        new.generated_by_user_id = "u-ghost".into();
        let err = insert(&conn, &new).unwrap_err();
        assert!(
            err.to_string().to_lowercase().contains("foreign key"),
            "expected FK violation, got: {err}"
        );
    }

    #[test]
    fn get_by_id_is_tenant_scoped() {
        let conn = open_test_conn();
        seed_org(&conn, "org-other", "other");
        let id = insert(&conn, &make_doc("org-other", RodoVariant::Short, 1)).unwrap();
        // Cross-tenant read must observe a miss, not the row.
        assert!(get_by_id(&conn, &id, "org-default").unwrap().is_none());
        assert!(get_by_id(&conn, &id, "org-other").unwrap().is_some());
    }

    #[test]
    fn list_by_org_filters_and_orders_desc() {
        let conn = open_test_conn();
        seed_membership(&conn, "org-default", "u-1");
        seed_org(&conn, "org-b", "b");
        let id_a = insert(&conn, &make_doc("org-default", RodoVariant::Short, 100)).unwrap();
        let id_b = insert(&conn, &make_doc("org-default", RodoVariant::Full, 300)).unwrap();
        let id_c = insert(&conn, &make_doc("org-default", RodoVariant::Standard, 200)).unwrap();
        let _ = insert(&conn, &make_doc("org-b", RodoVariant::Short, 999)).unwrap();

        let rows = list_by_org(&conn, "org-default", false).unwrap();
        let ids: Vec<_> = rows.iter().map(|d| d.id.clone()).collect();
        assert_eq!(ids, vec![id_b, id_c, id_a]);
    }

    #[test]
    fn list_by_org_hides_revoked_by_default() {
        let conn = open_test_conn();
        seed_membership(&conn, "org-default", "u-1");
        let id_a = insert(&conn, &make_doc("org-default", RodoVariant::Short, 100)).unwrap();
        let id_b = insert(&conn, &make_doc("org-default", RodoVariant::Full, 200)).unwrap();
        revoke(&conn, &id_a, "org-default", 500).unwrap();
        let active: Vec<_> = list_by_org(&conn, "org-default", false)
            .unwrap()
            .into_iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(active, vec![id_b.clone()]);
        let all: Vec<_> = list_by_org(&conn, "org-default", true)
            .unwrap()
            .into_iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(all, vec![id_b, id_a]);
    }

    #[test]
    fn set_signed_url_ref_persists_and_is_tenant_scoped() {
        let conn = open_test_conn();
        seed_membership(&conn, "org-default", "u-1");
        seed_org(&conn, "org-b", "b");
        let id = insert(&conn, &make_doc("org-default", RodoVariant::Short, 1)).unwrap();
        set_signed_url_ref(&conn, &id, "org-default", "ref-xyz").unwrap();
        let got = get_by_id(&conn, &id, "org-default").unwrap().unwrap();
        assert_eq!(got.signed_url_ref.as_deref(), Some("ref-xyz"));
        // Cross-tenant update must fail with no row affected.
        assert!(set_signed_url_ref(&conn, &id, "org-b", "ref-leak").is_err());
    }

    #[test]
    fn revoke_marks_row_and_is_tenant_scoped() {
        let conn = open_test_conn();
        seed_membership(&conn, "org-default", "u-1");
        seed_org(&conn, "org-b", "b");
        let id = insert(&conn, &make_doc("org-default", RodoVariant::Standard, 10)).unwrap();
        assert_eq!(
            revoke(&conn, &id, "org-default", 42).unwrap(),
            RevokeOutcome::FreshlyRevoked
        );
        let got = get_by_id(&conn, &id, "org-default").unwrap().unwrap();
        assert_eq!(got.revoked_at, Some(42));
        // Cross-tenant revoke matches zero rows — surfaced as AlreadyRevoked at
        // this layer; the service layer's `get_by_id` pre-check turns the
        // cross-tenant case into NotFound before we ever reach here.
        assert_eq!(
            revoke(&conn, &id, "org-b", 99).unwrap(),
            RevokeOutcome::AlreadyRevoked
        );
    }

    #[test]
    fn revoke_is_idempotent_second_call_preserves_timestamp() {
        let conn = open_test_conn();
        seed_membership(&conn, "org-default", "u-1");
        let id = insert(&conn, &make_doc("org-default", RodoVariant::Standard, 10)).unwrap();
        assert_eq!(
            revoke(&conn, &id, "org-default", 42).unwrap(),
            RevokeOutcome::FreshlyRevoked
        );
        assert_eq!(
            revoke(&conn, &id, "org-default", 99).unwrap(),
            RevokeOutcome::AlreadyRevoked
        );
        // Original timestamp preserved — the second revoke did not overwrite.
        let got = get_by_id(&conn, &id, "org-default").unwrap().unwrap();
        assert_eq!(got.revoked_at, Some(42));
    }
}
