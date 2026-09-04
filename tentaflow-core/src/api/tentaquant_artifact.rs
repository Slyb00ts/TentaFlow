// ===== File: api/tentaquant_artifact.rs — GET /tentaquant/artifacts/<ref> =====
//
// Serves ONE blob out of one laboratory's content store: a run's counts, its
// state vector or its recorded evolution (plan §9.4, §11.1 `RunArtifact`).
// This is the same deliberate exception the Project Studio export endpoint is
// — a 64 MiB state vector does not belong in a framed protocol message — and
// it is authenticated the same way: an HMAC signed URL, scope
// `UrlScope::TentaQuantArtifact`, minted by the protocol handler that already
// proved the caller may read that run.
//
// The reference IS the identity: `tqart_<org>_<instance>_<sha256>`. Org and
// instance ids are path segments, so both go through the sandbox's own
// validator before they are joined, and the blob is named by its content hash
// — a reference cannot point at anything but a file in that laboratory's
// `files/` directory, and cannot be forged without the signing key.
//
// Every fetch — ok, denied, missing, traversal — writes an `audit_log` row.
// An HMAC-only endpoint has no authenticated principal, so source IP and user
// agent are the forensic record.

use rusqlite::params;

use crate::api::project_studio_export::{ExportQuery, RequestContext};
use crate::db::DbPool;
use crate::services::signed_urls::{SignedUrlError, SignedUrlIssuer};

/// Path prefix the dashboard server routes on.
pub const ROUTE_PREFIX: &str = "/tentaquant/artifacts/";

/// Audit `risk_class`. A run artifact holds simulation numbers, not personal
/// data, but it IS somebody's work: it is classified as internal, not public.
const AUDIT_RISK_CLASS: &str = "internal";

/// Seconds a minted link stays valid — the ceiling of the scope, so the run
/// view can keep a tab open without re-minting on every redraw.
const URL_TTL_SECS: u64 = 3600;

/// A minted download link.
pub struct ArtifactUrl {
    pub url: String,
    pub expires_at_ms: u64,
}

/// Reference of one artifact. Deliberately built and parsed in one place, so
/// the format the issuer signs and the format the endpoint resolves cannot
/// drift apart.
pub fn artifact_ref(org_id: &str, instance_id: &str, sha256: &str) -> String {
    format!("tqart_{org_id}_{instance_id}_{sha256}")
}

/// Splits a reference back into its three parts, or `None` when it is not one.
///
/// Org and instance ids match `^[a-z0-9][a-z0-9-]{0,63}$` — no underscore —
/// which is what makes `_` an unambiguous separator here.
pub fn parse_ref(reference: &str) -> Option<(String, String, String)> {
    let rest = reference.strip_prefix("tqart_")?;
    let mut parts = rest.split('_');
    let org_id = parts.next()?;
    let instance_id = parts.next()?;
    let sha256 = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if crate::addon::fs_sandbox::validate_addon_id(org_id).is_err()
        || crate::addon::fs_sandbox::validate_addon_id(instance_id).is_err()
    {
        return None;
    }
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some((
        org_id.to_string(),
        instance_id.to_string(),
        sha256.to_string(),
    ))
}

/// Mints the signed URL a `RunArtifactResponse` carries.
pub fn issue(org_id: &str, instance_id: &str, sha256: &str) -> Result<ArtifactUrl, String> {
    let reference = artifact_ref(org_id, instance_id, sha256);
    if parse_ref(&reference).is_none() {
        return Err("artifact reference is not addressable".to_string());
    }
    let signed = crate::services::tentaquant_artifact_url_issuer()
        .issue(reference.clone(), URL_TTL_SECS)
        .map_err(|e| e.to_string())?;
    Ok(ArtifactUrl {
        url: format!("{ROUTE_PREFIX}{reference}?{}", signed.query_string()),
        expires_at_ms: signed.expiry_unix_ms,
    })
}

/// Result of the authorization step. The HTTP layer reads the blob after `Ok`.
#[derive(Debug)]
pub enum ArtifactOutcome {
    Ok,
    BadRequest(&'static str),
    Denied(SignedUrlError),
}

impl ArtifactOutcome {
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
            Self::Ok | Self::BadRequest(_) => "info",
            Self::Denied(_) => "warn",
        }
    }
}

/// Result of reading the blob.
#[derive(Debug)]
pub enum ArtifactFileOutcome {
    Ok {
        bytes: Vec<u8>,
    },
    /// The blob is gone — retention swept it, or the laboratory was removed.
    FileMissing,
    /// The resolved path is not a regular file inside the instance's store.
    PathTraversal,
    IoError,
}

impl ArtifactFileOutcome {
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
            Self::FileMissing => Some("artifact_missing_on_disk".to_string()),
            Self::PathTraversal => Some("path_outside_instance_store".to_string()),
            Self::IoError => Some("artifact_open_failed".to_string()),
        }
    }

    fn audit_severity(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "info",
            Self::FileMissing => "warn",
            Self::PathTraversal | Self::IoError => "error",
        }
    }

    fn audit_size(&self) -> Option<i64> {
        match self {
            Self::Ok { bytes } => Some(bytes.len() as i64),
            _ => None,
        }
    }
}

/// Verifies the signed URL. Every non-`Ok` outcome is audited here; `Ok` is
/// audited by [`read_artifact`], which is the step that touches the disk.
pub fn handle_artifact_url(
    path_ref: &str,
    query: &ExportQuery,
    issuer: &SignedUrlIssuer,
    pool: &DbPool,
    ctx: RequestContext<'_>,
) -> ArtifactOutcome {
    if parse_ref(path_ref).is_none() {
        return audit_and_return(
            pool,
            path_ref,
            ctx,
            ArtifactOutcome::BadRequest("invalid_ref_format"),
        );
    }
    let token = match query.token.as_deref() {
        Some(t) if !t.is_empty() => t,
        _ => {
            return audit_and_return(
                pool,
                path_ref,
                ctx,
                ArtifactOutcome::BadRequest("missing_token"),
            )
        }
    };
    let Some(exp_ms) = query.exp_ms else {
        return audit_and_return(
            pool,
            path_ref,
            ctx,
            ArtifactOutcome::BadRequest("missing_exp"),
        );
    };
    match query.ref_param.as_deref() {
        Some(reference) if reference == path_ref => {}
        Some(_) => {
            return audit_and_return(
                pool,
                path_ref,
                ctx,
                ArtifactOutcome::BadRequest("ref_path_mismatch"),
            )
        }
        None => {
            return audit_and_return(
                pool,
                path_ref,
                ctx,
                ArtifactOutcome::BadRequest("missing_ref"),
            )
        }
    }
    if let Err(e) = issuer.verify(path_ref, exp_ms, token) {
        return audit_and_return(pool, path_ref, ctx, ArtifactOutcome::Denied(e));
    }
    ArtifactOutcome::Ok
}

/// Reads the blob and audits the file step. Artifacts are bounded by the run
/// executor's own storage ceiling, so the body is read whole rather than
/// streamed — there is no range to resume across.
pub async fn read_artifact(
    pool: &DbPool,
    artifact_ref: &str,
    ctx: RequestContext<'_>,
) -> ArtifactFileOutcome {
    let outcome = read_artifact_inner(artifact_ref).await;
    write_audit_row(
        pool,
        artifact_ref,
        ctx,
        outcome.audit_result(),
        outcome.audit_reason(),
        outcome.audit_severity(),
        outcome.audit_size(),
    );
    outcome
}

async fn read_artifact_inner(artifact_ref: &str) -> ArtifactFileOutcome {
    let Some((org_id, instance_id, sha256)) = parse_ref(artifact_ref) else {
        return ArtifactFileOutcome::PathTraversal;
    };
    let Ok(data_dir) = crate::tentaquant::data_dir(&org_id, &instance_id) else {
        return ArtifactFileOutcome::PathTraversal;
    };
    let path = crate::tentaquant::cas::blob_path(&data_dir, &sha256);

    // Reject symlinks BEFORE any canonicalisation would resolve them.
    match tokio::fs::symlink_metadata(&path).await {
        Ok(m) if m.file_type().is_symlink() => return ArtifactFileOutcome::PathTraversal,
        Ok(m) if !m.is_file() => return ArtifactFileOutcome::PathTraversal,
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ArtifactFileOutcome::FileMissing
        }
        Err(_) => return ArtifactFileOutcome::IoError,
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => ArtifactFileOutcome::Ok { bytes },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ArtifactFileOutcome::FileMissing,
        Err(_) => ArtifactFileOutcome::IoError,
    }
}

/// Content type of one artifact, from the hash-named blob's own bytes: a CBOR
/// keyframe bundle is binary, everything else this executor writes is JSON.
pub fn content_type(bytes: &[u8]) -> &'static str {
    if bytes.first() == Some(&b'{') {
        "application/json"
    } else {
        "application/cbor"
    }
}

fn audit_and_return(
    pool: &DbPool,
    artifact_ref: &str,
    ctx: RequestContext<'_>,
    outcome: ArtifactOutcome,
) -> ArtifactOutcome {
    write_audit_row(
        pool,
        artifact_ref,
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
    artifact_ref: &str,
    ctx: RequestContext<'_>,
    result: &str,
    reason: Option<String>,
    severity: &str,
    size: Option<i64>,
) {
    let addon_id = parse_ref(artifact_ref).map(|(_, instance, _)| instance);
    let details = serde_json::json!({
        "ref": artifact_ref,
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
             VALUES (datetime('now'), NULL, ?1, 'tentaquant_artifact_url_access', \
                     'tentaquant_artifact', ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                addon_id,
                artifact_ref,
                result,
                reason,
                severity,
                AUDIT_RISK_CLASS,
                details,
            ],
        );
    }
}

fn truncate_ua(ua: &str) -> String {
    ua.chars().take(256).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn a_reference_round_trips_through_its_three_parts() {
        let reference = artifact_ref("org-default", "tentaquant-0a1b2c3d", SHA);
        let (org, instance, sha) = parse_ref(&reference).expect("parses");
        assert_eq!(org, "org-default");
        assert_eq!(instance, "tentaquant-0a1b2c3d");
        assert_eq!(sha, SHA);
    }

    /// The reference is interpolated into a filesystem path, so anything that
    /// is not exactly three path-safe parts has to be refused before it gets
    /// anywhere near `join`.
    #[test]
    fn a_reference_that_could_escape_the_store_is_refused() {
        for bad in [
            "",
            "tqart_org_instance",
            &format!("tqart_org_instance_{SHA}_extra"),
            &format!("tqart_../org_instance_{SHA}"),
            &format!("tqart_org_../instance_{SHA}"),
            &format!("tqart_org_instance_{}", "zz".repeat(32)),
            "tqart_org_instance_short",
            &format!("psexp_org_instance_{SHA}"),
        ] {
            assert!(parse_ref(bad).is_none(), "accepted: {bad}");
        }
    }

    #[test]
    fn content_type_follows_the_stored_bytes() {
        assert_eq!(content_type(b"{\"counts\":{}}"), "application/json");
        assert_eq!(content_type(&[0x82, 0x01]), "application/cbor");
        assert_eq!(content_type(&[]), "application/cbor");
    }
}
