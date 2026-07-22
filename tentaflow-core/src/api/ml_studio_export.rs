// ===== File: api/ml_studio_export.rs — GET /ml-studio/exports/<ref> signed-URL handler =====
//
// Serves ML Studio project export archives (zip, up to ~8 GB) to the browser's
// native downloader. Authentication is the HMAC signed-URL token from
// `services::signed_urls` with scope `UrlScope::MlStudioExport`; the `ref` path
// component and the `?ref=` query parameter must match. Multi-use within TTL is
// allowed (300 s - 7 days) so a paused download can be resumed via Range.
//
// Unlike `api::recording` there is no catalogue table: the ref IS the archive
// identity and resolves to `<ml_studio_exports_dir>/<ref>.zip`. Existence and
// containment are therefore decided entirely on the filesystem.
//
// Every fetch — ok, denied, missing, traversal — writes a row to `audit_log`
// with `action='ml_studio_export_url_access'`. The HTTP layer has no
// authenticated principal on an HMAC-only endpoint, so source IP + user agent
// are the forensic record.

use rusqlite::params;

use crate::db::DbPool;
use crate::paths::ml_studio_exports_dir;
use crate::services::signed_urls::{SignedUrlError, SignedUrlIssuer};

/// Audit `risk_class` for this endpoint. Export archives are project data, not
/// RODO-classified personal data, so they stay in the default bucket.
const AUDIT_RISK_CLASS: &str = "unclassified";

/// Strict reference-format gate. Export refs are `mlsexp_<uuid>` — anything
/// else is impossible to mint via the issuer and would only cost a futile HMAC
/// verify plus a filesystem probe. Because the ref is interpolated into the
/// on-disk filename, this regex is also the first line of defence against path
/// traversal: no `/`, `..` or NUL can survive it.
pub fn validate_ref_format(ref_id: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"^mlsexp_[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
        )
        .expect("ml studio export ref regex compiles")
    });
    re.is_match(ref_id)
}

/// Absolute path of the archive backing `export_ref`. Only meaningful for a ref
/// that already passed `validate_ref_format`.
pub fn export_archive_path(export_ref: &str) -> std::path::PathBuf {
    ml_studio_exports_dir().join(format!("{export_ref}.zip"))
}

/// Filename offered to the browser in `Content-Disposition`.
pub fn export_download_filename(export_ref: &str) -> String {
    format!("{export_ref}.zip")
}

/// Output of the pure authorization step. The HTTP layer opens the file async
/// after `Ok` and audits the file-access outcome separately.
#[derive(Debug)]
pub enum ExportOutcome {
    /// Token verified. The HTTP layer must now call `read_export_file`.
    Ok,
    /// Required query parameter missing, empty, duplicated, or unknown.
    BadRequest(&'static str),
    /// HMAC token rejected (forged / expired / scope mismatch).
    Denied(SignedUrlError),
}

impl ExportOutcome {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Ok => 200,
            Self::BadRequest(_) => 400,
            Self::Denied(_) => 403,
        }
    }

    fn audit_result(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::BadRequest(_) => "bad_request",
            Self::Denied(_) => "denied",
        }
    }

    fn audit_reason(&self) -> Option<String> {
        match self {
            Self::Ok => None,
            Self::BadRequest(why) => Some((*why).to_string()),
            Self::Denied(e) => Some(format!("{e}")),
        }
    }

    fn audit_severity(&self) -> &'static str {
        match self {
            Self::Ok => "info",
            Self::BadRequest(_) => "info",
            Self::Denied(_) => "warn",
        }
    }
}

/// Outcome of the async file-open step performed after `ExportOutcome::Ok`.
#[derive(Debug)]
pub enum ExportFileOutcome {
    /// Open handle + size — the HTTP layer STREAMS it (optionally a byte
    /// range), so an 8 GB archive never lands in memory.
    Ok { file: tokio::fs::File, size: u64 },
    /// Archive is gone (expired cache sweep, manual cleanup). The caller's
    /// signed URL is now stale, so 404 rather than 500.
    FileMissing,
    /// Resolved path escapes the exports base dir, or the target is a symlink.
    /// The archive writer never creates symlinks, so this means the cache dir
    /// was tampered with — 403.
    PathTraversal,
    /// Generic IO failure (permissions, FS error other than NotFound).
    IoError,
}

impl ExportFileOutcome {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Ok { .. } => 200,
            Self::FileMissing => 404,
            Self::PathTraversal => 403,
            Self::IoError => 500,
        }
    }

    fn audit_result(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::FileMissing => "not_found",
            Self::PathTraversal => "denied",
            Self::IoError => "error",
        }
    }

    fn audit_reason(&self) -> Option<String> {
        match self {
            Self::Ok { .. } => None,
            Self::FileMissing => Some("archive_missing_on_disk".to_string()),
            Self::PathTraversal => Some("path_outside_exports_base".to_string()),
            Self::IoError => Some("archive_open_failed".to_string()),
        }
    }

    fn audit_severity(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "info",
            Self::FileMissing => "warn",
            Self::PathTraversal => "error",
            Self::IoError => "error",
        }
    }

    fn audit_size(&self) -> Option<i64> {
        match self {
            Self::Ok { size, .. } => Some(*size as i64),
            _ => None,
        }
    }
}

/// Parsed query parameters for `/ml-studio/exports/<ref>?token=&exp=&ref=`.
/// Values are URL-decoded into owned strings — the issuer's `query_string()`
/// helper %-encodes `+` / `/` / `=` in the base64 token, so the raw query bytes
/// are not directly usable as the signature material.
#[derive(Debug, Default)]
pub struct ExportQuery {
    pub token: Option<String>,
    pub exp_ms: Option<u64>,
    pub ref_param: Option<String>,
}

/// Strict parse of `token=...&exp=...&ref=...`. Duplicate keys → error,
/// unknown keys → error. Invalid `exp` (non-numeric) → error. Trailing empty
/// piece from a leading/trailing `&` is tolerated.
pub fn parse_query(raw: &str) -> std::result::Result<ExportQuery, &'static str> {
    let mut q = ExportQuery::default();
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

/// Caller identity collected for the audit row. HMAC-only endpoints have no
/// authenticated principal, so this is the best we can do for forensics.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestContext<'a> {
    pub source_ip: Option<&'a str>,
    pub user_agent: Option<&'a str>,
}

/// Pure authorization handler. Verifies the signed URL; the HTTP layer opens
/// the archive off-handler (async). For every non-`Ok` outcome the audit row is
/// written here; for `Ok` the HTTP layer must call `read_export_file`, which
/// audits the file step.
pub fn handle_ml_studio_export_url(
    path_ref: &str,
    query: &ExportQuery,
    issuer: &SignedUrlIssuer,
    pool: &DbPool,
    ctx: RequestContext<'_>,
) -> ExportOutcome {
    if !validate_ref_format(path_ref) {
        return audit_and_return(
            pool,
            path_ref,
            ctx,
            ExportOutcome::BadRequest("invalid_ref_format"),
        );
    }
    let token = match query.token.as_deref() {
        Some(t) if !t.is_empty() => t,
        _ => {
            return audit_and_return(pool, path_ref, ctx, ExportOutcome::BadRequest("missing_token"))
        }
    };
    let exp_ms = match query.exp_ms {
        Some(v) => v,
        None => {
            return audit_and_return(pool, path_ref, ctx, ExportOutcome::BadRequest("missing_exp"))
        }
    };
    let ref_param = match query.ref_param.as_deref() {
        Some(r) if !r.is_empty() => r,
        _ => {
            return audit_and_return(pool, path_ref, ctx, ExportOutcome::BadRequest("missing_ref"))
        }
    };
    if ref_param != path_ref {
        return audit_and_return(
            pool,
            path_ref,
            ctx,
            ExportOutcome::BadRequest("ref_path_mismatch"),
        );
    }

    if let Err(e) = issuer.verify(path_ref, exp_ms, token) {
        return audit_and_return(pool, path_ref, ctx, ExportOutcome::Denied(e));
    }

    ExportOutcome::Ok
}

/// Open the export archive asynchronously and write one
/// `ml_studio_export_url_access` audit row mirroring the result.
pub async fn read_export_file(
    pool: &DbPool,
    export_ref: &str,
    ctx: RequestContext<'_>,
) -> ExportFileOutcome {
    let outcome = read_export_file_inner(&export_archive_path(export_ref)).await;
    audit_export_file_access(pool, export_ref, ctx, &outcome);
    outcome
}

/// Inner step kept separate so the containment / canonicalisation logic can be
/// exercised against arbitrary paths without touching `audit_log`.
async fn read_export_file_inner(file_path: &std::path::Path) -> ExportFileOutcome {
    // Reject symlinks BEFORE canonicalize — canonicalize would silently resolve
    // them and hand out whatever they point at.
    match tokio::fs::symlink_metadata(file_path).await {
        Ok(m) if m.file_type().is_symlink() => return ExportFileOutcome::PathTraversal,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ExportFileOutcome::FileMissing
        }
        Err(_) => return ExportFileOutcome::IoError,
        Ok(_) => {}
    }

    let canonical = match tokio::fs::canonicalize(file_path).await {
        Ok(p) => p,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ExportFileOutcome::FileMissing
        }
        Err(_) => return ExportFileOutcome::IoError,
    };

    if !path_within_exports_base(&canonical).await {
        return ExportFileOutcome::PathTraversal;
    }

    match tokio::fs::metadata(&canonical).await {
        Ok(m) if !m.is_file() => ExportFileOutcome::PathTraversal,
        Ok(m) => {
            // No size cap: the response is STREAMED (optionally a byte range),
            // so an 8 GB archive costs one chunk of memory, not its full length.
            let len = m.len();
            match tokio::fs::File::open(&canonical).await {
                Ok(f) => ExportFileOutcome::Ok { file: f, size: len },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    ExportFileOutcome::FileMissing
                }
                Err(_) => ExportFileOutcome::IoError,
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ExportFileOutcome::FileMissing,
        Err(_) => ExportFileOutcome::IoError,
    }
}

/// Strict containment: the canonical archive path must live under
/// `canonical(ml_studio_exports_dir())`. Fail-closed — unlike the recordings
/// endpoint there is no segment-scan fallback, because the base directory is
/// derived deterministically from `cache_dir()` and an unresolvable base means
/// no archive can legitimately exist yet.
async fn path_within_exports_base(canonical: &std::path::Path) -> bool {
    match tokio::fs::canonicalize(ml_studio_exports_dir()).await {
        Ok(base) => canonical.starts_with(&base),
        Err(_) => false,
    }
}

fn audit_export_file_access(
    pool: &DbPool,
    export_ref: &str,
    ctx: RequestContext<'_>,
    outcome: &ExportFileOutcome,
) {
    write_audit_row(
        pool,
        export_ref,
        ctx,
        outcome.audit_result(),
        outcome.audit_reason(),
        outcome.audit_severity(),
        outcome.audit_size(),
    );
}

fn audit_and_return(
    pool: &DbPool,
    export_ref: &str,
    ctx: RequestContext<'_>,
    outcome: ExportOutcome,
) -> ExportOutcome {
    write_audit_row(
        pool,
        export_ref,
        ctx,
        outcome.audit_result(),
        outcome.audit_reason(),
        outcome.audit_severity(),
        None,
    );
    outcome
}

fn write_audit_row(
    pool: &DbPool,
    export_ref: &str,
    ctx: RequestContext<'_>,
    result: &str,
    reason: Option<String>,
    severity: &str,
    size: Option<i64>,
) {
    let details = serde_json::json!({
        "ref": export_ref,
        "size": size,
        "source_ip": ctx.source_ip.unwrap_or(""),
        "user_agent": ctx.user_agent.map(truncate_ua).unwrap_or_default(),
    })
    .to_string();
    if let Ok(conn) = pool.write() {
        let _ = conn.execute(
            "INSERT INTO audit_log \
                (timestamp, user_id, addon_id, action, resource_type, resource_id, \
                 result, error_message, severity, risk_class, details) \
             VALUES (datetime('now'), NULL, NULL, 'ml_studio_export_url_access', \
                     'ml_studio_export', ?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                export_ref,
                result,
                reason,
                severity,
                AUDIT_RISK_CLASS,
                details,
            ],
        );
    }
}

/// Cap user-agent to 256 chars — clients can send arbitrary headers and we
/// don't want to bloat `audit_log.details` JSON.
fn truncate_ua(ua: &str) -> String {
    ua.chars().take(256).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REF_A: &str = "mlsexp_550e8400-e29b-41d4-a716-446655440000";

    #[test]
    fn test_parse_query_basic() {
        let q = parse_query(&format!("token=abc&exp=1234&ref={REF_A}")).expect("ok");
        assert_eq!(q.token.as_deref(), Some("abc"));
        assert_eq!(q.exp_ms, Some(1234));
        assert_eq!(q.ref_param.as_deref(), Some(REF_A));
    }

    #[test]
    fn test_parse_query_rejects_unknown_and_duplicates() {
        assert_eq!(
            parse_query("foo=bar&token=t&exp=1&ref=r").unwrap_err(),
            "unknown_query_key"
        );
        assert_eq!(
            parse_query("token=a&token=b&exp=1&ref=r").unwrap_err(),
            "duplicate_token"
        );
        assert_eq!(
            parse_query("token=a&exp=1&exp=2&ref=r").unwrap_err(),
            "duplicate_exp"
        );
        assert_eq!(
            parse_query("token=a&exp=1&ref=r1&ref=r2").unwrap_err(),
            "duplicate_ref"
        );
        assert_eq!(
            parse_query("token=a&exp=nope&ref=r").unwrap_err(),
            "invalid_exp"
        );
    }

    #[test]
    fn test_parse_query_url_decodes_token() {
        let q = parse_query("token=AB%3D%3D&exp=99&ref=x").expect("ok");
        assert_eq!(q.token.as_deref(), Some("AB=="));
    }

    #[test]
    fn test_validate_ref_format() {
        assert!(validate_ref_format(REF_A));
        assert!(!validate_ref_format(""));
        assert!(!validate_ref_format("mlsexp_not-a-uuid"));
        assert!(!validate_ref_format("../../etc/passwd"));
        assert!(!validate_ref_format(&format!("{REF_A}/../../etc/passwd")));
        // Refs minted for other scopes must not be reachable here.
        assert!(!validate_ref_format(
            "snap_550e8400-e29b-41d4-a716-446655440000"
        ));
    }

    #[test]
    fn test_outcome_status_codes() {
        assert_eq!(ExportOutcome::Ok.http_status(), 200);
        assert_eq!(ExportOutcome::BadRequest("x").http_status(), 400);
        assert_eq!(
            ExportOutcome::Denied(SignedUrlError::InvalidSignature).http_status(),
            403
        );
        assert_eq!(
            ExportOutcome::Denied(SignedUrlError::Expired).http_status(),
            403
        );
    }

    #[test]
    fn test_file_outcome_status_codes() {
        assert_eq!(ExportFileOutcome::FileMissing.http_status(), 404);
        assert_eq!(ExportFileOutcome::PathTraversal.http_status(), 403);
        assert_eq!(ExportFileOutcome::IoError.http_status(), 500);
    }

    #[test]
    fn test_archive_path_and_filename() {
        assert_eq!(export_download_filename(REF_A), format!("{REF_A}.zip"));
        assert_eq!(
            export_archive_path(REF_A),
            ml_studio_exports_dir().join(format!("{REF_A}.zip"))
        );
    }

    #[tokio::test]
    async fn test_read_inner_rejects_path_outside_base() {
        // A real, readable file that is definitely not under the exports dir.
        let outcome = read_export_file_inner(std::path::Path::new("/etc/hostname")).await;
        assert!(
            matches!(
                outcome,
                ExportFileOutcome::PathTraversal | ExportFileOutcome::FileMissing
            ),
            "outside-base path must never be served, got {outcome:?}"
        );
    }

    #[tokio::test]
    async fn test_read_inner_missing_file() {
        let outcome = read_export_file_inner(&export_archive_path(
            "mlsexp_00000000-0000-0000-0000-000000000000",
        ))
        .await;
        assert!(matches!(outcome, ExportFileOutcome::FileMissing));
    }
}
