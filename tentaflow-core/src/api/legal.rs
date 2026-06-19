// =============================================================================
// File: api/legal.rs — GET /legal/<doc_id> signed-URL handler (F2 P8.c)
// =============================================================================
//
// Browser-facing endpoint that returns the bytes of an `legal_documents` PDF.
// Authentication is an HMAC signed-URL token minted via the `legal_url` issuer
// (scope `UrlScope::LegalUrl`). Query shape mirrors `/recordings` plus two
// extra fields the legal binding needs:
//
//   /legal/<doc_id>?token=<b64>&exp=<ms>&org=<uuid>&nonce=<uuid>
//
// The HMAC is computed over the composite `<doc_id>|<org_id>|<nonce>` so a
// token minted for org A cannot be replayed against the same doc_id from org
// B — defence in depth on top of the row-level tenant filter.
//
// Every fetch — ok, denied, expired, missing, revoked — writes a B-class
// `legal.download` row to `audit_log`. The Merkle chain is computed via the
// shared `compute_chain_for_insert` helper so every audit row participates in
// the F1b P4 hash chain.

use std::path::Path;
use std::sync::Arc;

use rusqlite::params;

use crate::audit::chain::{compute_chain_for_insert, AuditRowHashInput};
use crate::db::legal_documents::get_by_id;
use crate::db::DbPool;
use crate::services::legal::verify_legal_token;
use crate::services::signed_urls::{SignedUrlError, SignedUrlIssuer};

/// Hard cap on the legal-PDF response. 16 MiB matches the recording response
/// cap — a typical RODO PDF is < 200 KiB so a 16 MiB ceiling means an
/// integrity error (or a tampered DB pointing at an unrelated file) cannot
/// blow Tokio worker memory.
pub const MAX_LEGAL_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Strict UUIDv4 format gate — every legal doc_id and org_id is a v4 UUID
/// (enforced by the DB CHECK on `legal_documents.id` + `organizations.org_id`).
/// Rejecting non-UUID inputs up-front avoids a futile DB select + HMAC verify
/// and prevents a hostile `/legal/../../etc/passwd` from reaching the row
/// lookup at all.
pub fn validate_uuid_v4(s: &str) -> bool {
    match uuid::Uuid::parse_str(s) {
        Ok(u) => u.get_version_num() == 4 && s.len() == 36,
        Err(_) => false,
    }
}

/// Parsed query string for the legal endpoint. The four fields are
/// individually URL-decoded so the issuer's `query_string()` percent-encoding
/// (token `=` → `%3D`) round-trips losslessly.
#[derive(Debug, Default)]
pub struct LegalQuery {
    pub token: Option<String>,
    pub exp_ms: Option<u64>,
    pub org: Option<String>,
    pub nonce: Option<String>,
}

/// Strict parse: duplicate keys → error, unknown keys → error, non-numeric
/// `exp` → error. Empty pieces from leading/trailing `&` are tolerated.
pub fn parse_query(raw: &str) -> std::result::Result<LegalQuery, &'static str> {
    let mut q = LegalQuery::default();
    if raw.is_empty() {
        return Ok(q);
    }
    for piece in raw.split('&') {
        if piece.is_empty() {
            continue;
        }
        let mut it = piece.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        let decoded = urlencoding::decode(v)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| v.to_string());
        match k {
            "token" => {
                if q.token.is_some() {
                    return Err("duplicate_token");
                }
                q.token = Some(decoded);
            }
            "exp" => {
                if q.exp_ms.is_some() {
                    return Err("duplicate_exp");
                }
                let parsed: u64 = decoded.parse().map_err(|_| "invalid_exp")?;
                q.exp_ms = Some(parsed);
            }
            "org" => {
                if q.org.is_some() {
                    return Err("duplicate_org");
                }
                q.org = Some(decoded);
            }
            "nonce" => {
                if q.nonce.is_some() {
                    return Err("duplicate_nonce");
                }
                q.nonce = Some(decoded);
            }
            _ => return Err("unknown_query_key"),
        }
    }
    Ok(q)
}

/// Caller identity for the audit row. The legal endpoint is HMAC-only, so the
/// only forensic anchor is the source IP + user-agent header.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestContext<'a> {
    pub source_ip: Option<&'a str>,
    pub user_agent: Option<&'a str>,
}

/// Outcome of authorization + DB resolution. The file read is performed by
/// `read_legal_file` after `Ok` is returned.
#[derive(Debug)]
pub enum LegalOutcome {
    Ok {
        org_id: String,
        pdf_path: String,
        content_hash: String,
        generated_at: i64,
    },
    BadRequest(&'static str),
    Denied(SignedUrlError),
    NotFound,
    /// The row exists but has been soft-deleted via `legal.revoke`. Distinct
    /// from `NotFound` so audit can record the difference; HTTP status is the
    /// same (403) to keep the wire shape uniform.
    Revoked,
    InternalError(&'static str),
}

impl LegalOutcome {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Ok { .. } => 200,
            Self::BadRequest(_) => 400,
            Self::Denied(_) => 403,
            Self::Revoked => 403,
            Self::NotFound => 404,
            Self::InternalError(_) => 500,
        }
    }

    fn audit_result(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::BadRequest(_) => "bad_request",
            Self::Denied(_) => "denied",
            Self::Revoked => "denied",
            Self::NotFound => "not_found",
            Self::InternalError(_) => "error",
        }
    }

    fn audit_reason(&self) -> Option<String> {
        match self {
            Self::Ok { .. } => None,
            Self::BadRequest(why) => Some((*why).to_string()),
            Self::Denied(e) => Some(format!("{e}")),
            Self::Revoked => Some("revoked".to_string()),
            Self::NotFound => Some("not_found".to_string()),
            Self::InternalError(why) => Some((*why).to_string()),
        }
    }
}

/// Outcome of the async file-read step after authorization.
#[derive(Debug)]
pub enum LegalFileOutcome {
    Ok { bytes: Vec<u8> },
    FileMissing,
    FileTooLarge,
    IoError,
    PathTraversal,
}

impl LegalFileOutcome {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Ok { .. } => 200,
            Self::FileMissing => 404,
            Self::FileTooLarge => 413,
            Self::PathTraversal => 403,
            Self::IoError => 500,
        }
    }
}

/// Pure authorization handler. The HTTP layer reads bytes off-handler so the
/// Tokio worker is not blocked on `std::fs::read`. For every non-`Ok` outcome
/// the audit row is written here; for `Ok` the HTTP layer calls
/// `audit_legal_file_access` after the read step.
pub fn handle_legal_url(
    path_doc_id: &str,
    query: &LegalQuery,
    issuer: &Arc<SignedUrlIssuer>,
    pool: &DbPool,
    ctx: RequestContext<'_>,
) -> LegalOutcome {
    if !validate_uuid_v4(path_doc_id) {
        return audit_and_return(
            pool,
            path_doc_id,
            None,
            ctx,
            LegalOutcome::BadRequest("invalid_doc_id"),
        );
    }
    let token = match query.token.as_deref() {
        Some(t) if !t.is_empty() => t,
        _ => {
            return audit_and_return(
                pool,
                path_doc_id,
                None,
                ctx,
                LegalOutcome::BadRequest("missing_token"),
            )
        }
    };
    let exp_ms = match query.exp_ms {
        Some(v) => v,
        None => {
            return audit_and_return(
                pool,
                path_doc_id,
                None,
                ctx,
                LegalOutcome::BadRequest("missing_exp"),
            )
        }
    };
    let org_id = match query.org.as_deref() {
        Some(o) if !o.is_empty() => o,
        _ => {
            return audit_and_return(
                pool,
                path_doc_id,
                None,
                ctx,
                LegalOutcome::BadRequest("missing_org"),
            )
        }
    };
    let nonce = match query.nonce.as_deref() {
        Some(n) if !n.is_empty() => n,
        _ => {
            return audit_and_return(
                pool,
                path_doc_id,
                None,
                ctx,
                LegalOutcome::BadRequest("missing_nonce"),
            )
        }
    };
    if !validate_uuid_v4(org_id) {
        return audit_and_return(
            pool,
            path_doc_id,
            Some(org_id),
            ctx,
            LegalOutcome::BadRequest("invalid_org"),
        );
    }

    if let Err(e) = verify_legal_token(issuer, path_doc_id, org_id, nonce, exp_ms, token) {
        return audit_and_return(
            pool,
            path_doc_id,
            Some(org_id),
            ctx,
            LegalOutcome::Denied(e),
        );
    }

    let conn_guard = match pool.read() {
        Ok(g) => g,
        Err(_) => {
            return audit_and_return(
                pool,
                path_doc_id,
                Some(org_id),
                ctx,
                LegalOutcome::InternalError("db_poisoned"),
            )
        }
    };
    let row = match get_by_id(&conn_guard, path_doc_id, org_id) {
        Ok(Some(r)) => r,
        Ok(None) => {
            drop(conn_guard);
            return audit_and_return(pool, path_doc_id, Some(org_id), ctx, LegalOutcome::NotFound);
        }
        Err(e) => {
            drop(conn_guard);
            tracing::warn!("legal handle_legal_url db error: {e}");
            return audit_and_return(
                pool,
                path_doc_id,
                Some(org_id),
                ctx,
                LegalOutcome::InternalError("db_error"),
            );
        }
    };
    drop(conn_guard);

    if row.revoked_at.is_some() {
        return audit_and_return(pool, path_doc_id, Some(org_id), ctx, LegalOutcome::Revoked);
    }
    LegalOutcome::Ok {
        org_id: row.org_id,
        pdf_path: row.pdf_path,
        content_hash: row.content_hash,
        generated_at: row.generated_at,
    }
}

/// Async file-read step. Mirrors `read_recording_file` — strict symlink
/// rejection, canonicalization, size cap. The legal-root containment check
/// pins the result under `crate::paths::legal_root_dir()`.
pub async fn read_legal_file(
    pool: &DbPool,
    doc_id: &str,
    org_id: &str,
    file_path: &str,
    ctx: RequestContext<'_>,
) -> LegalFileOutcome {
    let outcome = read_legal_file_inner(file_path).await;
    audit_legal_file_access(pool, doc_id, org_id, ctx, &outcome);
    outcome
}

async fn read_legal_file_inner(file_path: &str) -> LegalFileOutcome {
    // Reject symlinks BEFORE we open anything. `symlink_metadata` does not
    // follow the final component, so a symlink at the leaf is caught here.
    match tokio::fs::symlink_metadata(file_path).await {
        Ok(m) if m.file_type().is_symlink() => return LegalFileOutcome::PathTraversal,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LegalFileOutcome::FileMissing,
        Err(_) => return LegalFileOutcome::IoError,
        Ok(_) => {}
    }

    // Open the file FIRST and hold the descriptor for the rest of the routine.
    // Reads below come from this held fd, not from a fresh `read(path)` — so
    // an attacker who swaps the path for a symlink after our containment check
    // would mutate a different inode than the one we are about to serve.
    // `OpenOptions::open` with default flags follows symlinks, but the prior
    // `symlink_metadata` rejected that case; any swap that lands between these
    // two syscalls would still have to win the race AND survive `metadata()`
    // on the held fd returning a non-regular file type below.
    let file = match tokio::fs::File::open(file_path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LegalFileOutcome::FileMissing,
        Err(_) => return LegalFileOutcome::IoError,
    };

    // fd-anchored metadata: the kind/size we use comes from the inode the open
    // file descriptor points at, not from a re-resolved path. A regular file
    // is required — a swap to a directory, FIFO, or device is rejected.
    let fd_meta = match file.metadata().await {
        Ok(m) => m,
        Err(_) => return LegalFileOutcome::IoError,
    };
    if !fd_meta.is_file() {
        return LegalFileOutcome::PathTraversal;
    }
    let len = fd_meta.len();
    if len > MAX_LEGAL_RESPONSE_BYTES {
        return LegalFileOutcome::FileTooLarge;
    }

    // Containment check via the canonical path. Run AFTER we already have the
    // fd so the bytes we will return come from the inode validated above; a
    // racing symlink swap between this canonicalize and the upcoming read
    // cannot redirect us because `read_to_end` operates on `file` (held fd).
    let canonical = match tokio::fs::canonicalize(file_path).await {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LegalFileOutcome::FileMissing,
        Err(_) => return LegalFileOutcome::IoError,
    };
    if !path_within_legal_root(&canonical).await {
        return LegalFileOutcome::PathTraversal;
    }

    use tokio::io::AsyncReadExt;
    let mut file = file;
    let mut bytes = Vec::with_capacity(len as usize);
    match file.read_to_end(&mut bytes).await {
        Ok(_) => LegalFileOutcome::Ok { bytes },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => LegalFileOutcome::FileMissing,
        Err(_) => LegalFileOutcome::IoError,
    }
}

async fn path_within_legal_root(canonical: &Path) -> bool {
    let base = crate::paths::legal_root_dir();
    if let Ok(canonical_base) = tokio::fs::canonicalize(&base).await {
        return canonical.starts_with(&canonical_base);
    }
    false
}

fn audit_and_return(
    pool: &DbPool,
    doc_id: &str,
    org_id: Option<&str>,
    ctx: RequestContext<'_>,
    outcome: LegalOutcome,
) -> LegalOutcome {
    let result = outcome.audit_result();
    let reason = outcome.audit_reason();
    let severity = if matches!(&outcome, LegalOutcome::Denied(_) | LegalOutcome::Revoked) {
        "warn"
    } else if matches!(&outcome, LegalOutcome::InternalError(_)) {
        "error"
    } else {
        "info"
    };
    write_audit_row(
        pool,
        doc_id,
        org_id,
        ctx,
        "legal.download",
        result,
        reason,
        severity,
        None,
    );
    outcome
}

fn audit_legal_file_access(
    pool: &DbPool,
    doc_id: &str,
    org_id: &str,
    ctx: RequestContext<'_>,
    outcome: &LegalFileOutcome,
) {
    let (result, reason, severity, size): (
        &'static str,
        Option<String>,
        &'static str,
        Option<i64>,
    ) = match outcome {
        LegalFileOutcome::Ok { bytes } => ("ok", None, "info", Some(bytes.len() as i64)),
        LegalFileOutcome::FileMissing => ("not_found", Some("file_missing".into()), "warn", None),
        LegalFileOutcome::FileTooLarge => (
            "error",
            Some("file_exceeds_response_cap".into()),
            "error",
            None,
        ),
        LegalFileOutcome::PathTraversal => (
            "denied",
            Some("path_outside_legal_root".into()),
            "error",
            None,
        ),
        LegalFileOutcome::IoError => ("error", Some("file_read_failed".into()), "error", None),
    };
    write_audit_row(
        pool,
        doc_id,
        Some(org_id),
        ctx,
        "legal.download",
        result,
        reason,
        severity,
        size,
    );
}

#[allow(clippy::too_many_arguments)]
fn write_audit_row(
    pool: &DbPool,
    doc_id: &str,
    org_id: Option<&str>,
    ctx: RequestContext<'_>,
    action: &str,
    result: &str,
    reason: Option<String>,
    severity: &str,
    size_bytes: Option<i64>,
) {
    let details = serde_json::json!({
        "doc_id": doc_id,
        "size": size_bytes,
        "source_ip": ctx.source_ip.unwrap_or(""),
        "user_agent": ctx.user_agent.map(truncate_ua).unwrap_or_default(),
    })
    .to_string();
    let Ok(conn) = pool.write() else { return };
    let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let resource_type = Some("legal_document");
    let resource_id = Some(doc_id);
    let result_opt = Some(result);
    let severity_opt = Some(severity);
    let risk_class = "B";
    let error_message = reason.as_deref();
    let hash_input = AuditRowHashInput {
        user_id: None,
        addon_id: None,
        instance_id: None,
        action,
        resource: None,
        resource_type,
        resource_id,
        result: result_opt,
        error_message,
        details: Some(details.as_str()),
        ip_address: ctx.source_ip,
        node_id: None,
        severity: severity_opt,
        risk_class,
        related_claim_id: None,
        request_id: None,
        timestamp: &timestamp,
    };
    let Ok((prev_hash, hash)) = compute_chain_for_insert(&conn, &hash_input) else {
        return;
    };
    let _ = conn.execute(
        "INSERT INTO audit_log \
            (timestamp, action, resource_type, resource_id, result, error_message, \
             severity, risk_class, details, org_id, ip_address, prev_hash, hash) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            timestamp,
            action,
            resource_type,
            resource_id,
            result_opt,
            error_message,
            severity_opt,
            risk_class,
            details,
            org_id,
            ctx.source_ip,
            prev_hash,
            hash,
        ],
    );
}

fn truncate_ua(ua: &str) -> String {
    ua.chars().take(256).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_happy() {
        let q = parse_query("token=abc&exp=1234&org=11111111-1111-4111-8111-111111111111&nonce=n")
            .expect("ok");
        assert_eq!(q.token.as_deref(), Some("abc"));
        assert_eq!(q.exp_ms, Some(1234));
        assert_eq!(
            q.org.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(q.nonce.as_deref(), Some("n"));
    }

    #[test]
    fn parse_query_rejects_unknown_key() {
        assert_eq!(parse_query("foo=bar").unwrap_err(), "unknown_query_key");
    }

    #[test]
    fn parse_query_rejects_duplicate_org() {
        assert_eq!(
            parse_query("token=a&exp=1&org=x&org=y&nonce=n").unwrap_err(),
            "duplicate_org"
        );
    }

    #[test]
    fn parse_query_url_decodes_token() {
        let q = parse_query("token=AB%3D%3D&exp=99&org=o&nonce=n").expect("ok");
        assert_eq!(q.token.as_deref(), Some("AB=="));
    }

    #[test]
    fn validate_uuid_v4_round_trip() {
        assert!(validate_uuid_v4("11111111-1111-4111-8111-111111111111"));
        assert!(!validate_uuid_v4("not-a-uuid"));
        assert!(!validate_uuid_v4("../../etc/passwd"));
        // V1 / V3 UUIDs (wrong version bit) are rejected.
        assert!(!validate_uuid_v4("11111111-1111-1111-8111-111111111111"));
    }

    #[test]
    fn outcome_http_status_codes() {
        assert_eq!(LegalOutcome::BadRequest("x").http_status(), 400);
        assert_eq!(LegalOutcome::NotFound.http_status(), 404);
        assert_eq!(
            LegalOutcome::Denied(SignedUrlError::InvalidSignature).http_status(),
            403
        );
        assert_eq!(LegalOutcome::Revoked.http_status(), 403);
        assert_eq!(LegalOutcome::InternalError("x").http_status(), 500);
    }
}
