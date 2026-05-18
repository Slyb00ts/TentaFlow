// =============================================================================
// Plik: db/legal_documents.rs
// Opis: F2 P8.a repo for the `legal_documents` table — RODO/GDPR PDF artifacts.
//       Every read / write filters by `org_id` so a caller can never touch
//       another tenant's documents (org isolation invariant). The variant
//       enum is owned by `services::legal::RodoVariant`.
// =============================================================================

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::services::legal::RodoVariant;

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

pub fn insert(conn: &Connection, doc: &LegalDocument) -> Result<()> {
    let affected = conn.execute(
        "INSERT INTO legal_documents \
            (id, org_id, variant, generated_at, generated_by_user_id, \
             content_hash, pdf_path, signed_url_ref, revoked_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            doc.id,
            doc.org_id,
            doc.variant.as_str(),
            doc.generated_at,
            doc.generated_by_user_id,
            doc.content_hash,
            doc.pdf_path,
            doc.signed_url_ref,
            doc.revoked_at,
        ],
    )?;
    if affected != 1 {
        return Err(anyhow!(
            "legal_documents insert affected {} rows (expected 1)",
            affected
        ));
    }
    Ok(())
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
pub fn get_by_id(
    conn: &Connection,
    doc_id: &str,
    org_id: &str,
) -> Result<Option<LegalDocument>> {
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
pub fn set_signed_url_ref(
    conn: &Connection,
    doc_id: &str,
    org_id: &str,
    ref_: &str,
) -> Result<()> {
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
pub fn revoke(
    conn: &Connection,
    doc_id: &str,
    org_id: &str,
    now_ms: i64,
) -> Result<()> {
    let affected = conn.execute(
        "UPDATE legal_documents SET revoked_at = ?1 \
         WHERE id = ?2 AND org_id = ?3",
        params![now_ms, doc_id, org_id],
    )?;
    if affected != 1 {
        return Err(anyhow!(
            "legal_documents revoke: no row for id={doc_id} org_id={org_id}"
        ));
    }
    Ok(())
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

    fn make_doc(id: &str, org: &str, variant: RodoVariant, ts: i64) -> LegalDocument {
        LegalDocument {
            id: id.into(),
            org_id: org.into(),
            variant,
            generated_at: ts,
            generated_by_user_id: "u-1".into(),
            content_hash: "deadbeef".into(),
            pdf_path: format!("/tmp/{id}.pdf"),
            signed_url_ref: None,
            revoked_at: None,
        }
    }

    fn seed_org(conn: &Connection, id: &str, slug: &str) {
        conn.execute(
            "INSERT INTO organizations (org_id, name, slug, status, created_at) \
             VALUES (?1, ?2, ?3, 'active', '2026-01-01T00:00:00Z')",
            params![id, id, slug],
        )
        .unwrap();
    }

    #[test]
    fn insert_and_get_round_trip() {
        let conn = open_test_conn();
        let doc = make_doc("doc-1", "org-default", RodoVariant::Standard, 1000);
        insert(&conn, &doc).unwrap();
        let got = get_by_id(&conn, "doc-1", "org-default").unwrap().unwrap();
        assert_eq!(got, doc);
    }

    #[test]
    fn get_by_id_is_tenant_scoped() {
        let conn = open_test_conn();
        seed_org(&conn, "org-other", "other");
        let doc = make_doc("doc-x", "org-other", RodoVariant::Short, 1);
        insert(&conn, &doc).unwrap();
        // Cross-tenant read must observe a miss, not the row.
        assert!(get_by_id(&conn, "doc-x", "org-default").unwrap().is_none());
        assert!(get_by_id(&conn, "doc-x", "org-other").unwrap().is_some());
    }

    #[test]
    fn list_by_org_filters_and_orders_desc() {
        let conn = open_test_conn();
        seed_org(&conn, "org-b", "b");
        insert(&conn, &make_doc("a", "org-default", RodoVariant::Short, 100)).unwrap();
        insert(&conn, &make_doc("b", "org-default", RodoVariant::Full, 300)).unwrap();
        insert(&conn, &make_doc("c", "org-default", RodoVariant::Standard, 200)).unwrap();
        insert(&conn, &make_doc("z", "org-b", RodoVariant::Short, 999)).unwrap();

        let rows = list_by_org(&conn, "org-default", false).unwrap();
        let ids: Vec<_> = rows.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "c", "a"]);
    }

    #[test]
    fn list_by_org_hides_revoked_by_default() {
        let conn = open_test_conn();
        insert(&conn, &make_doc("a", "org-default", RodoVariant::Short, 100)).unwrap();
        insert(&conn, &make_doc("b", "org-default", RodoVariant::Full, 200)).unwrap();
        revoke(&conn, "a", "org-default", 500).unwrap();
        let active: Vec<_> = list_by_org(&conn, "org-default", false)
            .unwrap()
            .into_iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(active, vec!["b"]);
        let all: Vec<_> = list_by_org(&conn, "org-default", true)
            .unwrap()
            .into_iter()
            .map(|d| d.id)
            .collect();
        assert_eq!(all, vec!["b", "a"]);
    }

    #[test]
    fn set_signed_url_ref_persists_and_is_tenant_scoped() {
        let conn = open_test_conn();
        seed_org(&conn, "org-b", "b");
        insert(&conn, &make_doc("a", "org-default", RodoVariant::Short, 1)).unwrap();
        set_signed_url_ref(&conn, "a", "org-default", "ref-xyz").unwrap();
        let got = get_by_id(&conn, "a", "org-default").unwrap().unwrap();
        assert_eq!(got.signed_url_ref.as_deref(), Some("ref-xyz"));
        // Cross-tenant update must fail with no row affected.
        assert!(set_signed_url_ref(&conn, "a", "org-b", "ref-leak").is_err());
    }

    #[test]
    fn revoke_marks_row_and_is_tenant_scoped() {
        let conn = open_test_conn();
        seed_org(&conn, "org-b", "b");
        insert(&conn, &make_doc("a", "org-default", RodoVariant::Standard, 10)).unwrap();
        revoke(&conn, "a", "org-default", 42).unwrap();
        let got = get_by_id(&conn, "a", "org-default").unwrap().unwrap();
        assert_eq!(got.revoked_at, Some(42));
        // Cross-tenant revoke must not succeed.
        assert!(revoke(&conn, "a", "org-b", 99).is_err());
    }
}
