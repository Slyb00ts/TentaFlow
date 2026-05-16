// =============================================================================
// File: api/frames.rs — GET /frames/<ref> multi-use signed-URL handler
// =============================================================================
//
// Returns the raw RGB24 bytes of a frame currently in the in-memory LRU
// (`services::frame_storage`). Multi-use within the issuer TTL (60-600 s);
// the LRU entry is NOT consumed (peek semantics) so an addon can dereference
// the same URL several times before the frame is evicted or the TTL elapses.
//
// Auth is the HMAC signed URL issued by `services::signed_urls` scope
// `UrlScope::FrameUrl`. Frame metadata (width / height / pixel format /
// timestamp) ships in response headers analogous to `/core/frame/pickup`.

use rusqlite::params;

use crate::db::DbPool;
use crate::services::frame_storage::{FramePixelFormat, FrameStorage, RawFrameRef, StoredFrame};
use crate::services::signed_urls::{SignedUrlError, SignedUrlIssuer};

pub const HDR_FRAME_WIDTH: &str = "X-Frame-Width";
pub const HDR_FRAME_HEIGHT: &str = "X-Frame-Height";
pub const HDR_FRAME_PIXEL_FORMAT: &str = "X-Frame-Pixel-Format";
pub const HDR_FRAME_TS_MS: &str = "X-Frame-Timestamp-Ms";
pub const HDR_FRAME_PTS: &str = "X-Frame-Pts";

#[derive(Debug)]
pub enum FrameOutcome {
    Ok {
        bytes: std::sync::Arc<[u8]>,
        width: u32,
        height: u32,
        pixel_format: &'static str,
        timestamp_unix_ms: u64,
        pts: Option<u64>,
    },
    BadRequest(&'static str),
    Denied(SignedUrlError),
    NotFound,
}

impl FrameOutcome {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Ok { .. } => 200,
            Self::BadRequest(_) => 400,
            Self::Denied(_) => 403,
            Self::NotFound => 404,
        }
    }

    fn audit_result(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::BadRequest(_) => "bad_request",
            Self::Denied(_) => "denied",
            Self::NotFound => "not_found",
        }
    }

    fn audit_reason(&self) -> Option<String> {
        match self {
            Self::Ok { .. } => None,
            Self::BadRequest(why) => Some((*why).to_string()),
            Self::Denied(e) => Some(format!("{e}")),
            Self::NotFound => Some("frame_evicted_or_unknown".to_string()),
        }
    }
}

#[derive(Debug, Default)]
pub struct FrameQuery {
    pub token: Option<String>,
    pub exp_ms: Option<u64>,
    pub ref_param: Option<String>,
}

/// Strict parse. Duplicate keys, unknown keys, or a non-numeric `exp` all
/// yield an error string suitable for a 400-class `error_message`.
pub fn parse_query(raw: &str) -> std::result::Result<FrameQuery, &'static str> {
    let mut q = FrameQuery::default();
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

pub fn handle_frame_url(
    path_ref: &str,
    query: &FrameQuery,
    issuer: &SignedUrlIssuer,
    storage: &FrameStorage,
    pool: &DbPool,
) -> FrameOutcome {
    let token = match query.token.as_deref() {
        Some(t) if !t.is_empty() => t,
        _ => return audit_and_return(pool, path_ref, FrameOutcome::BadRequest("missing_token")),
    };
    let exp_ms = match query.exp_ms {
        Some(v) => v,
        None => return audit_and_return(pool, path_ref, FrameOutcome::BadRequest("missing_exp")),
    };
    let ref_param = match query.ref_param.as_deref() {
        Some(r) if !r.is_empty() => r,
        _ => return audit_and_return(pool, path_ref, FrameOutcome::BadRequest("missing_ref")),
    };
    if ref_param != path_ref {
        return audit_and_return(pool, path_ref, FrameOutcome::BadRequest("ref_path_mismatch"));
    }

    if let Err(e) = issuer.verify(path_ref, exp_ms, token) {
        return audit_and_return(pool, path_ref, FrameOutcome::Denied(e));
    }

    let stored: StoredFrame = match storage.get(&RawFrameRef::from_string(path_ref.to_string())) {
        Some(s) => s,
        None => return audit_and_return(pool, path_ref, FrameOutcome::NotFound),
    };
    let pf = match stored.metadata.pixel_format {
        FramePixelFormat::Rgb24 => "rgb24",
    };
    let outcome = FrameOutcome::Ok {
        bytes: stored.data,
        width: stored.metadata.width,
        height: stored.metadata.height,
        pixel_format: pf,
        timestamp_unix_ms: stored.metadata.timestamp_unix_ms,
        pts: stored.metadata.pts,
    };
    audit_and_return(pool, path_ref, outcome)
}

fn audit_and_return(pool: &DbPool, frame_ref: &str, outcome: FrameOutcome) -> FrameOutcome {
    let result = outcome.audit_result();
    let reason = outcome.audit_reason();
    let severity = match &outcome {
        FrameOutcome::Denied(_) | FrameOutcome::NotFound => "warn",
        _ => "info",
    };
    let size = match &outcome {
        FrameOutcome::Ok { bytes, .. } => Some(bytes.len() as i64),
        _ => None,
    };
    let details = serde_json::json!({ "ref": frame_ref, "size": size }).to_string();
    if let Ok(conn) = pool.lock() {
        // FrameUrl access has no addon identity at the HTTP layer (HMAC-only
        // auth), so addon_id stays NULL. Risk class B matches host-fn
        // `frame_url_v1` issuance.
        let _ = conn.execute(
            "INSERT INTO audit_log \
                (timestamp, user_id, addon_id, action, resource_type, resource_id, \
                 result, error_message, severity, risk_class, details) \
             VALUES (datetime('now'), NULL, NULL, 'frame_url_access', \
                     'frame', ?1, ?2, ?3, ?4, 'B', ?5)",
            params![frame_ref, result, reason, severity, details],
        );
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_query_full() {
        let q = parse_query("token=a&exp=99&ref=frame_xyz").expect("ok");
        assert_eq!(q.token.as_deref(), Some("a"));
        assert_eq!(q.exp_ms, Some(99));
        assert_eq!(q.ref_param.as_deref(), Some("frame_xyz"));
    }

    #[test]
    fn test_parse_query_rejects_duplicate_and_unknown() {
        assert_eq!(parse_query("token=a&token=b").unwrap_err(), "duplicate_token");
        assert_eq!(parse_query("token=a&extra=x").unwrap_err(), "unknown_query_key");
        assert_eq!(parse_query("token=a&exp=nope").unwrap_err(), "invalid_exp");
    }

    #[test]
    fn test_status_codes() {
        assert_eq!(FrameOutcome::BadRequest("x").http_status(), 400);
        assert_eq!(FrameOutcome::NotFound.http_status(), 404);
        assert_eq!(
            FrameOutcome::Denied(SignedUrlError::InvalidSignature).http_status(),
            403
        );
    }
}
