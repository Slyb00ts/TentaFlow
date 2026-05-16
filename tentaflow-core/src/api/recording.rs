// =============================================================================
// File: api/recording.rs — GET /recordings/<ref> signed-URL handler
// =============================================================================
//
// Public, addon-facing endpoint that returns the bytes of a snapshot PNG or
// segment MP4 catalogued in the `recordings` table. Authentication is the
// HMAC signed-URL token from `services::signed_urls` with scope
// `UrlScope::Recording`; the `ref` path component and the `?ref=` query
// parameter must match. Multi-use within TTL is allowed (60-3600 s).
//
// Every fetch — ok, denied, expired, missing, purged — writes a row to
// `audit_log` with `action='recording_url_access'` and `risk_class` copied
// from the row's `retention_class`. This keeps the chain-of-custody bound to
// the addon that originally saved the artifact even though the HTTP layer
// itself has no addon identity.

#![cfg(feature = "camera")]

use rusqlite::params;

use crate::db::repository::{get_recording_by_ref, RecordingRow};
use crate::db::DbPool;
use crate::services::signed_urls::{SignedUrlError, SignedUrlIssuer};

/// Hard cap on the file size we are willing to return in a single response.
/// Recordings larger than this are treated as integrity errors — F1a does not
/// stream, so a single oversized blob would block the runtime and bloat memory.
pub const MAX_RECORDING_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Output of the pure authorization step. The HTTP layer reads the file async
/// after `Ok` and audits the file-access outcome separately.
#[derive(Debug)]
pub enum RecordingOutcome {
    /// Token verified + DB row present. HTTP layer must now fs::metadata +
    /// fs::read the file and audit the file-access outcome via
    /// `audit_recording_file_access`.
    Ok {
        content_type: &'static str,
        hash_sha256: String,
        created_at: i64,
        file_size_bytes: i64,
        file_path: String,
        retention_class: String,
        owner_addon_id: String,
    },
    /// Required query parameter missing, empty, duplicated, or unknown.
    BadRequest(&'static str),
    /// HMAC token rejected (forged / expired / scope mismatch).
    Denied(SignedUrlError),
    /// Recording row absent or already purged.
    NotFound,
    /// DB lookup or other internal failure before the file read step.
    InternalError(&'static str),
}

impl RecordingOutcome {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Ok { .. } => 200,
            Self::BadRequest(_) => 400,
            Self::Denied(SignedUrlError::Expired) => 403,
            Self::Denied(_) => 403,
            Self::NotFound => 404,
            Self::InternalError(_) => 500,
        }
    }

    fn audit_result(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::BadRequest(_) => "bad_request",
            Self::Denied(_) => "denied",
            Self::NotFound => "not_found",
            Self::InternalError(_) => "error",
        }
    }

    fn audit_reason(&self) -> Option<String> {
        match self {
            Self::Ok { .. } => None,
            Self::BadRequest(why) => Some((*why).to_string()),
            Self::Denied(e) => Some(format!("{e}")),
            Self::NotFound => Some("not_found_or_purged".to_string()),
            Self::InternalError(why) => Some((*why).to_string()),
        }
    }
}

/// Outcome of the async file-read step performed after `RecordingOutcome::Ok`.
/// Wire-mapped by the HTTP layer to 200 / 404 / 413 / 500.
#[derive(Debug)]
pub enum RecordingFileOutcome {
    Ok { bytes: Vec<u8> },
    /// File row exists in DB but the on-disk file is gone — wire-mapped to 404
    /// rather than 500 because the caller's signed URL is now stale.
    FileMissing,
    /// On-disk file is larger than `MAX_RECORDING_RESPONSE_BYTES`.
    FileTooLarge,
    /// On-disk file size disagrees with the size recorded in the DB row —
    /// corruption / tampering signal, surfaces as 500 with audit error.
    FileIntegrityError,
    /// Generic IO failure (permissions, FS error other than NotFound).
    IoError,
}

impl RecordingFileOutcome {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Ok { .. } => 200,
            Self::FileMissing => 404,
            Self::FileTooLarge => 413,
            Self::FileIntegrityError | Self::IoError => 500,
        }
    }

    fn audit_result(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::FileMissing => "not_found",
            Self::FileTooLarge => "error",
            Self::FileIntegrityError | Self::IoError => "error",
        }
    }

    fn audit_reason(&self) -> Option<String> {
        match self {
            Self::Ok { .. } => None,
            Self::FileMissing => Some("file_missing_on_disk".to_string()),
            Self::FileTooLarge => Some("file_exceeds_response_cap".to_string()),
            Self::FileIntegrityError => Some("file_size_mismatches_db".to_string()),
            Self::IoError => Some("file_read_failed".to_string()),
        }
    }

    fn audit_severity(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "info",
            Self::FileMissing => "warn",
            Self::FileIntegrityError | Self::FileTooLarge | Self::IoError => "error",
        }
    }

    fn audit_size(&self) -> Option<i64> {
        match self {
            Self::Ok { bytes } => Some(bytes.len() as i64),
            _ => None,
        }
    }
}

/// Parsed query parameters for `/recordings/<ref>?token=&exp=&ref=`. Values
/// are URL-decoded into owned strings — the issuer's `query_string()` helper
/// %-encodes `+` / `/` / `=` in the base64 token, so the raw query bytes are
/// not directly usable as the signature material.
#[derive(Debug, Default)]
pub struct RecordingQuery {
    pub token: Option<String>,
    pub exp_ms: Option<u64>,
    pub ref_param: Option<String>,
}

/// Strict parse of `token=...&exp=...&ref=...`. Duplicate keys → error,
/// unknown keys → error. Invalid `exp` (non-numeric) → error. Trailing empty
/// piece from a leading/trailing `&` is tolerated. URL-decodes each value
/// best-effort (matches the issuer's `query_string()` percent-encoding).
pub fn parse_query(raw: &str) -> std::result::Result<RecordingQuery, &'static str> {
    let mut q = RecordingQuery::default();
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
            "ref" => {
                if q.ref_param.is_some() {
                    return Err("duplicate_ref");
                }
                q.ref_param = Some(decoded);
            }
            _ => return Err("unknown_query_key"),
        }
    }
    Ok(q)
}

/// Pure authorization handler. Verifies the signed URL and resolves the DB
/// row; the HTTP layer reads the file off-handler (async) so the runtime is
/// not blocked by `std::fs::read`. For every non-`Ok` outcome the audit row is
/// written here; for `Ok` the HTTP layer must call
/// `audit_recording_file_access` after the file read step.
pub fn handle_recording_url(
    path_ref: &str,
    query: &RecordingQuery,
    issuer: &SignedUrlIssuer,
    pool: &DbPool,
) -> RecordingOutcome {
    let token = match query.token.as_deref() {
        Some(t) if !t.is_empty() => t,
        _ => return audit_and_return(pool, path_ref, "Unclassified", None, RecordingOutcome::BadRequest("missing_token")),
    };
    let exp_ms = match query.exp_ms {
        Some(v) => v,
        None => return audit_and_return(pool, path_ref, "Unclassified", None, RecordingOutcome::BadRequest("missing_exp")),
    };
    let ref_param = match query.ref_param.as_deref() {
        Some(r) if !r.is_empty() => r,
        _ => return audit_and_return(pool, path_ref, "Unclassified", None, RecordingOutcome::BadRequest("missing_ref")),
    };
    if ref_param != path_ref {
        return audit_and_return(pool, path_ref, "Unclassified", None, RecordingOutcome::BadRequest("ref_path_mismatch"));
    }

    if let Err(e) = issuer.verify(path_ref, exp_ms, token) {
        return audit_and_return(pool, path_ref, "Unclassified", None, RecordingOutcome::Denied(e));
    }

    let row: RecordingRow = match get_recording_by_ref(pool, path_ref) {
        Ok(Some(r)) => r,
        Ok(None) => return audit_and_return(pool, path_ref, "Unclassified", None, RecordingOutcome::NotFound),
        Err(_) => return audit_and_return(pool, path_ref, "Unclassified", None, RecordingOutcome::InternalError("db_error")),
    };

    let content_type = match row.kind.as_str() {
        "snapshot" => "image/png",
        "segment" => "video/mp4",
        _ => "application/octet-stream",
    };
    RecordingOutcome::Ok {
        content_type,
        hash_sha256: row.hash_sha256,
        created_at: row.created_at,
        file_size_bytes: row.file_size_bytes,
        file_path: row.file_path,
        retention_class: row.retention_class,
        owner_addon_id: row.owner_addon_id,
    }
}

/// Read the recording bytes off disk asynchronously, enforcing the response
/// size cap and DB↔FS size integrity. Writes one `recording_url_access` audit
/// row mirroring the file-read result.
pub async fn read_recording_file(
    pool: &DbPool,
    recording_ref: &str,
    file_path: &str,
    retention_class: &str,
    owner_addon_id: &str,
    expected_size: i64,
) -> RecordingFileOutcome {
    let outcome = match tokio::fs::metadata(file_path).await {
        Ok(m) => {
            let len = m.len();
            if len > MAX_RECORDING_RESPONSE_BYTES {
                RecordingFileOutcome::FileTooLarge
            } else if expected_size >= 0 && len != expected_size as u64 {
                RecordingFileOutcome::FileIntegrityError
            } else {
                match tokio::fs::read(file_path).await {
                    Ok(b) => RecordingFileOutcome::Ok { bytes: b },
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        RecordingFileOutcome::FileMissing
                    }
                    Err(_) => RecordingFileOutcome::IoError,
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => RecordingFileOutcome::FileMissing,
        Err(_) => RecordingFileOutcome::IoError,
    };
    audit_recording_file_access(pool, recording_ref, retention_class, owner_addon_id, &outcome);
    outcome
}

fn audit_recording_file_access(
    pool: &DbPool,
    recording_ref: &str,
    retention_class: &str,
    addon_id: &str,
    outcome: &RecordingFileOutcome,
) {
    let result = outcome.audit_result();
    let reason = outcome.audit_reason();
    let severity = outcome.audit_severity();
    let size = outcome.audit_size();
    let details = serde_json::json!({
        "ref": recording_ref,
        "size": size,
    })
    .to_string();
    if let Ok(conn) = pool.lock() {
        let _ = conn.execute(
            "INSERT INTO audit_log \
                (timestamp, user_id, addon_id, action, resource_type, resource_id, \
                 result, error_message, severity, risk_class, details) \
             VALUES (datetime('now'), NULL, ?1, 'recording_url_access', \
                     'recording', ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                addon_id,
                recording_ref,
                result,
                reason,
                severity,
                retention_class,
                details,
            ],
        );
    }
}

fn audit_and_return(
    pool: &DbPool,
    recording_ref: &str,
    retention_class: &str,
    addon_id: Option<&str>,
    outcome: RecordingOutcome,
) -> RecordingOutcome {
    let result = outcome.audit_result();
    let reason = outcome.audit_reason();
    let severity = if matches!(&outcome, RecordingOutcome::Denied(_) | RecordingOutcome::NotFound) {
        "warn"
    } else if matches!(&outcome, RecordingOutcome::InternalError(_)) {
        "error"
    } else {
        "info"
    };
    let details = serde_json::json!({
        "ref": recording_ref,
        "size": Option::<i64>::None,
    })
    .to_string();
    if let Ok(conn) = pool.lock() {
        let _ = conn.execute(
            "INSERT INTO audit_log \
                (timestamp, user_id, addon_id, action, resource_type, resource_id, \
                 result, error_message, severity, risk_class, details) \
             VALUES (datetime('now'), NULL, ?1, 'recording_url_access', \
                     'recording', ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                addon_id,
                recording_ref,
                result,
                reason,
                severity,
                retention_class,
                details,
            ],
        );
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_query_basic() {
        let q = parse_query("token=abc&exp=1234&ref=snap_x").expect("ok");
        assert_eq!(q.token.as_deref(), Some("abc"));
        assert_eq!(q.exp_ms, Some(1234));
        assert_eq!(q.ref_param.as_deref(), Some("snap_x"));
    }

    #[test]
    fn test_parse_query_missing() {
        let q = parse_query("token=abc").expect("ok");
        assert_eq!(q.token.as_deref(), Some("abc"));
        assert!(q.exp_ms.is_none());
        assert!(q.ref_param.is_none());
    }

    #[test]
    fn test_parse_query_unknown_key_rejected() {
        let err = parse_query("foo=bar&token=t&exp=1&ref=r").unwrap_err();
        assert_eq!(err, "unknown_query_key");
    }

    #[test]
    fn test_parse_query_duplicate_token_rejected() {
        let err = parse_query("token=a&token=b&exp=1&ref=r").unwrap_err();
        assert_eq!(err, "duplicate_token");
    }

    #[test]
    fn test_parse_query_duplicate_ref_rejected() {
        let err = parse_query("token=a&exp=1&ref=r1&ref=r2").unwrap_err();
        assert_eq!(err, "duplicate_ref");
    }

    #[test]
    fn test_parse_query_invalid_exp_rejected() {
        let err = parse_query("token=a&exp=notanumber&ref=r").unwrap_err();
        assert_eq!(err, "invalid_exp");
    }

    #[test]
    fn test_parse_query_url_decodes_token() {
        let q = parse_query("token=AB%3D%3D&exp=99&ref=snap_x").expect("ok");
        assert_eq!(q.token.as_deref(), Some("AB=="));
    }

    #[test]
    fn test_outcome_status_codes() {
        assert_eq!(RecordingOutcome::BadRequest("x").http_status(), 400);
        assert_eq!(RecordingOutcome::NotFound.http_status(), 404);
        assert_eq!(
            RecordingOutcome::Denied(SignedUrlError::InvalidSignature).http_status(),
            403
        );
        assert_eq!(
            RecordingOutcome::Denied(SignedUrlError::Expired).http_status(),
            403
        );
        assert_eq!(
            RecordingOutcome::InternalError("x").http_status(),
            500
        );
    }

    #[test]
    fn test_file_outcome_status_codes() {
        assert_eq!(RecordingFileOutcome::FileMissing.http_status(), 404);
        assert_eq!(RecordingFileOutcome::FileTooLarge.http_status(), 413);
        assert_eq!(RecordingFileOutcome::FileIntegrityError.http_status(), 500);
        assert_eq!(RecordingFileOutcome::IoError.http_status(), 500);
        assert_eq!(RecordingFileOutcome::Ok { bytes: vec![] }.http_status(), 200);
    }
}
