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

/// Output of the pure handler — HTTP layer maps to a `Response`.
#[derive(Debug)]
pub enum RecordingOutcome {
    Ok {
        bytes: Vec<u8>,
        content_type: &'static str,
        hash_sha256: String,
        created_at: i64,
        file_size_bytes: i64,
    },
    /// Required query parameter missing or empty.
    BadRequest(&'static str),
    /// HMAC token rejected (forged / expired / scope mismatch).
    Denied(SignedUrlError),
    /// Recording row absent or already purged.
    NotFound,
    /// Filesystem read failed (file disappeared between DB row and read).
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

/// Parse `token=...&exp=...&ref=...` and URL-decode each value. Unknown keys
/// are ignored; repeated keys take the first value.
pub fn parse_query(raw: &str) -> RecordingQuery {
    let mut q = RecordingQuery::default();
    for piece in raw.split('&') {
        let mut it = piece.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        let decoded = urlencoding::decode(v).map(|c| c.into_owned()).unwrap_or_else(|_| v.to_string());
        match k {
            "token" if q.token.is_none() => q.token = Some(decoded),
            "exp" if q.exp_ms.is_none() => q.exp_ms = decoded.parse::<u64>().ok(),
            "ref" if q.ref_param.is_none() => q.ref_param = Some(decoded),
            _ => {}
        }
    }
    q
}

/// Pure handler. The hyper layer extracts `path_ref` from the URL path,
/// parses the query, and maps the outcome to a Response (bytes + headers).
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

    let retention_class = row.retention_class.clone();
    let owner_addon_id = row.owner_addon_id.clone();

    let bytes = match std::fs::read(&row.file_path) {
        Ok(b) => b,
        Err(_) => return audit_and_return(pool, path_ref, &retention_class, Some(&owner_addon_id), RecordingOutcome::InternalError("file_read_failed")),
    };
    let content_type = match row.kind.as_str() {
        "snapshot" => "image/png",
        "segment" => "video/mp4",
        _ => "application/octet-stream",
    };
    let outcome = RecordingOutcome::Ok {
        bytes,
        content_type,
        hash_sha256: row.hash_sha256.clone(),
        created_at: row.created_at,
        file_size_bytes: row.file_size_bytes,
    };
    audit_and_return(pool, path_ref, &retention_class, Some(&owner_addon_id), outcome)
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
    let size = match &outcome {
        RecordingOutcome::Ok { file_size_bytes, .. } => Some(*file_size_bytes),
        _ => None,
    };
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
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_query_basic() {
        let q = parse_query("token=abc&exp=1234&ref=snap_x");
        assert_eq!(q.token.as_deref(), Some("abc"));
        assert_eq!(q.exp_ms, Some(1234));
        assert_eq!(q.ref_param.as_deref(), Some("snap_x"));
    }

    #[test]
    fn test_parse_query_missing() {
        let q = parse_query("token=abc");
        assert_eq!(q.token.as_deref(), Some("abc"));
        assert!(q.exp_ms.is_none());
        assert!(q.ref_param.is_none());
    }

    #[test]
    fn test_parse_query_extra_keys_ignored() {
        let q = parse_query("foo=bar&token=t&exp=1&ref=r&junk=x");
        assert_eq!(q.token.as_deref(), Some("t"));
    }

    #[test]
    fn test_parse_query_url_decodes_token() {
        let q = parse_query("token=AB%3D%3D&exp=99&ref=snap_x");
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
}
