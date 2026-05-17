// ============ File: services/policy/repo.rs — DB layer for policy_claims ============
//
// Pure CRUD on the `policy_claims` + `policy_claim_signatures` tables.
// All timestamps are UTC ISO-8601 ("YYYY-MM-DDTHH:MM:SSZ"); callers pass
// pre-formatted strings so unit tests can pin "now" deterministically.
// The engine layer never writes — only `repo` does.

use rusqlite::{params, OptionalExtension};

use super::error::{PolicyError, Result};
use crate::db::DbPool;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewClaim {
    pub claim_id: String,
    pub claim_type: String,
    pub label: String,
    pub subject: Option<String>,
    pub scope: Option<String>,
    pub document_uri: Option<String>,
    pub scope_addon_id: Option<String>,
    pub scope_namespace: Option<String>,
    pub valid_from: String,
    pub valid_until: String,
    pub issued_by_user: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSignature {
    pub claim_id: String,
    pub signer_role: String,
    pub signer_user: String,
    pub signed_at: String,
    pub signature_b64: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRow {
    pub claim_id: String,
    pub claim_type: String,
    pub label: String,
    pub subject: Option<String>,
    pub scope: Option<String>,
    pub document_uri: Option<String>,
    pub scope_addon_id: Option<String>,
    pub scope_namespace: Option<String>,
    pub valid_from: String,
    pub valid_until: String,
    pub revoked_at: Option<String>,
    pub revoked_reason: Option<String>,
    pub issued_by_user: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimSignatureRow {
    pub claim_id: String,
    pub signer_role: String,
    pub signer_user: String,
    pub signed_at: String,
    pub signature_b64: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub claim_type: Option<String>,
    /// When true, exclude rows where revoked_at IS NOT NULL or valid_until < now.
    pub active_only: bool,
    /// UTC ISO-8601 "now" used by `active_only` time comparison.
    pub now_utc: Option<String>,
}

fn map_db<E: std::fmt::Display>(e: E) -> PolicyError {
    PolicyError::DbError(e.to_string())
}

pub fn insert_claim(pool: &DbPool, claim: &NewClaim) -> Result<()> {
    let conn = pool.lock().map_err(|e| PolicyError::DbError(e.to_string()))?;
    conn.execute(
        "INSERT INTO policy_claims (claim_id, claim_type, label, subject, scope, document_uri, \
            scope_addon_id, scope_namespace, valid_from, valid_until, issued_by_user, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            claim.claim_id,
            claim.claim_type,
            claim.label,
            claim.subject,
            claim.scope,
            claim.document_uri,
            claim.scope_addon_id,
            claim.scope_namespace,
            claim.valid_from,
            claim.valid_until,
            claim.issued_by_user,
            claim.created_at,
        ],
    )
    .map_err(map_db)?;
    Ok(())
}

pub fn insert_signature(pool: &DbPool, sig: &NewSignature) -> Result<()> {
    let conn = pool.lock().map_err(|e| PolicyError::DbError(e.to_string()))?;
    conn.execute(
        "INSERT INTO policy_claim_signatures (claim_id, signer_role, signer_user, signed_at, signature_b64) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            sig.claim_id,
            sig.signer_role,
            sig.signer_user,
            sig.signed_at,
            sig.signature_b64,
        ],
    )
    .map_err(map_db)?;
    Ok(())
}

pub fn delete_signature(
    pool: &DbPool,
    claim_id: &str,
    signer_role: &str,
    signer_user: &str,
) -> Result<bool> {
    let conn = pool.lock().map_err(|e| PolicyError::DbError(e.to_string()))?;
    let n = conn
        .execute(
            "DELETE FROM policy_claim_signatures WHERE claim_id = ?1 AND signer_role = ?2 AND signer_user = ?3",
            params![claim_id, signer_role, signer_user],
        )
        .map_err(map_db)?;
    Ok(n > 0)
}

pub fn get_claim(pool: &DbPool, claim_id: &str) -> Result<Option<ClaimRow>> {
    let conn = pool.lock().map_err(|e| PolicyError::DbError(e.to_string()))?;
    conn.query_row(
        "SELECT claim_id, claim_type, label, subject, scope, document_uri, scope_addon_id, \
                scope_namespace, valid_from, valid_until, revoked_at, revoked_reason, \
                issued_by_user, created_at \
         FROM policy_claims WHERE claim_id = ?1",
        params![claim_id],
        |row| {
            Ok(ClaimRow {
                claim_id: row.get(0)?,
                claim_type: row.get(1)?,
                label: row.get(2)?,
                subject: row.get(3)?,
                scope: row.get(4)?,
                document_uri: row.get(5)?,
                scope_addon_id: row.get(6)?,
                scope_namespace: row.get(7)?,
                valid_from: row.get(8)?,
                valid_until: row.get(9)?,
                revoked_at: row.get(10)?,
                revoked_reason: row.get(11)?,
                issued_by_user: row.get(12)?,
                created_at: row.get(13)?,
            })
        },
    )
    .optional()
    .map_err(map_db)
}

pub fn list_signatures(pool: &DbPool, claim_id: &str) -> Result<Vec<ClaimSignatureRow>> {
    let conn = pool.lock().map_err(|e| PolicyError::DbError(e.to_string()))?;
    let mut stmt = conn
        .prepare(
            "SELECT claim_id, signer_role, signer_user, signed_at, signature_b64 \
             FROM policy_claim_signatures WHERE claim_id = ?1 ORDER BY signer_role, signer_user",
        )
        .map_err(map_db)?;
    let rows = stmt
        .query_map(params![claim_id], |row| {
            Ok(ClaimSignatureRow {
                claim_id: row.get(0)?,
                signer_role: row.get(1)?,
                signer_user: row.get(2)?,
                signed_at: row.get(3)?,
                signature_b64: row.get(4)?,
            })
        })
        .map_err(map_db)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(map_db)?);
    }
    Ok(out)
}

pub fn list_claims(pool: &DbPool, filter: &ListFilter) -> Result<Vec<ClaimRow>> {
    let conn = pool.lock().map_err(|e| PolicyError::DbError(e.to_string()))?;
    // Build query dynamically — kept simple (one optional WHERE clause per
    // filter field). For active_only we filter post-fetch when no `now_utc`
    // is supplied (defensive: never silently drop rows because of a missing
    // wall clock).
    let mut sql = String::from(
        "SELECT claim_id, claim_type, label, subject, scope, document_uri, scope_addon_id, \
                scope_namespace, valid_from, valid_until, revoked_at, revoked_reason, \
                issued_by_user, created_at FROM policy_claims",
    );
    let mut where_clauses: Vec<String> = Vec::new();
    let mut bind: Vec<String> = Vec::new();
    if let Some(t) = &filter.claim_type {
        where_clauses.push(format!("claim_type = ?{}", bind.len() + 1));
        bind.push(t.clone());
    }
    if filter.active_only {
        where_clauses.push("revoked_at IS NULL".to_string());
        if let Some(now) = &filter.now_utc {
            where_clauses.push(format!(
                "valid_from <= ?{} AND valid_until >= ?{}",
                bind.len() + 1,
                bind.len() + 2
            ));
            bind.push(now.clone());
            bind.push(now.clone());
        }
    }
    if !where_clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&where_clauses.join(" AND "));
    }
    sql.push_str(" ORDER BY created_at DESC");

    let mut stmt = conn.prepare(&sql).map_err(map_db)?;
    let params_dyn: Vec<&dyn rusqlite::ToSql> =
        bind.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params_dyn.iter().copied()), |row| {
            Ok(ClaimRow {
                claim_id: row.get(0)?,
                claim_type: row.get(1)?,
                label: row.get(2)?,
                subject: row.get(3)?,
                scope: row.get(4)?,
                document_uri: row.get(5)?,
                scope_addon_id: row.get(6)?,
                scope_namespace: row.get(7)?,
                valid_from: row.get(8)?,
                valid_until: row.get(9)?,
                revoked_at: row.get(10)?,
                revoked_reason: row.get(11)?,
                issued_by_user: row.get(12)?,
                created_at: row.get(13)?,
            })
        })
        .map_err(map_db)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(map_db)?);
    }
    Ok(out)
}

pub fn revoke_claim(
    pool: &DbPool,
    claim_id: &str,
    reason: &str,
    revoked_at: &str,
) -> Result<bool> {
    let conn = pool.lock().map_err(|e| PolicyError::DbError(e.to_string()))?;
    let n = conn
        .execute(
            "UPDATE policy_claims SET revoked_at = ?1, revoked_reason = ?2 \
             WHERE claim_id = ?3 AND revoked_at IS NULL",
            params![revoked_at, reason, claim_id],
        )
        .map_err(map_db)?;
    Ok(n > 0)
}
