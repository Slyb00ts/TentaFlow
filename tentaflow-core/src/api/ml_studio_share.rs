// =============================================================================
// File: api/ml_studio_share.rs — GET /ml-studio/share/<project_id>/manifest +
//       GET /ml-studio/share/<project_id>/archive cross-instance sharing
// =============================================================================
//
// HTTPS distribution of a whole ML Studio project to a node that is NOT paired
// in the same mesh. Mirrors `api::model_bundle` exactly, with two independent
// auth modes:
//
// 1. Signed URLs (paired/ad-hoc sharing): an admin on the serving node mints a
//    manifest URL (scope `UrlScope::MlStudioExport`, ref = the project id). The
//    pulling node fetches the manifest JSON and then downloads the on-demand
//    export archive through a per-ref signed URL derived from the manifest
//    token's remaining lifetime. The archive token signs the composite
//    `<project_id>/archive` resource, so a manifest token cannot be replayed as
//    the archive token.
//
// 2. API keys (UNPAIRED instances): `Authorization: Bearer <key>` with the same
//    verifier pipeline as `/v1` plus an explicit `resource_permissions` allow
//    rule on `('ml_studio_export', <project_id>)` (default-DENY). API-key
//    manifests return a token-less archive path; the client repeats the same
//    Bearer header on the archive GET, and the archive endpoint re-checks the
//    key's project scope on every request.
//
// Unlike model bundles (curated files already on disk) the export archive is
// built ON DEMAND from the project's DB rows + on-disk datasets by
// `ml_studio::project_archive::build_export`, cached under
// `paths::ml_studio_share_cache_dir()`, and rebuilt when stale. Archives reach
// ~8 GB, so the archive endpoint supports HTTP Range and a global build
// semaphore prevents concurrent share requests from triggering many
// simultaneous multi-gigabyte builds (a DoS surface).
//
// Every manifest/archive fetch writes one `audit_log` row with
// `action='ml_studio_share_url_access'`. Tokens and keys are never logged
// (only the resolved key uid).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use rusqlite::params;

use crate::api::frames::FrameQuery;
use crate::db::DbPool;
use crate::ml_studio::project_archive::{self, build_export, ExportOptions, ARCHIVE_VERSION};
use crate::paths::ml_studio_share_cache_dir;
use crate::services::signed_urls::{SignedUrl, SignedUrlError, SignedUrlIssuer};

/// ACL resource type carrying API-key project scopes in `resource_permissions`,
/// and the signed-URL scope's ref namespace. Shared with the local
/// `UrlScope::MlStudioExport` download so one key scope covers both flows.
pub const ML_STUDIO_SHARE_RESOURCE_TYPE: &str = "ml_studio_export";

/// Export knobs for a shared archive: models are included (the point of sharing
/// a project is to re-train/serve it elsewhere), training history is not (it is
/// node-local telemetry, not needed to reopen the project).
const SHARE_EXPORT_OPTIONS: ExportOptions = ExportOptions {
    include_models: true,
    include_history: false,
};

/// Staleness ceiling for a cached archive. Annotations are stored as COCO JSON
/// ON DISK (not in the DB), so an editor save does NOT bump `projects.updated_at`
/// — this TTL bounds how long such an out-of-band edit can be missed. Within the
/// TTL a cached archive is reused as long as `projects.updated_at` has not moved
/// past its build time; both conditions must hold for a cache hit.
const SHARE_CACHE_MAX_AGE_SECS: u64 = 300;

/// Global bound on concurrent archive builds. Each build can be a multi-gigabyte
/// zip, so an unbounded fan-out of share requests would be a disk + CPU DoS
/// lever — two at a time mirrors the model-bundle hash semaphore.
static BUILD_CONCURRENCY: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

/// Per-project singleflight: concurrent first requests for the SAME project wait
/// for one build instead of each spawning its own. Bounded by the number of
/// distinct shared projects.
static BUILD_SINGLEFLIGHT: OnceLock<
    parking_lot::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
> = OnceLock::new();

/// SHA-256 cache keyed by project id — a hit requires the on-disk (mtime, size)
/// pair to match, so a rebuilt archive is re-hashed exactly once.
static HASH_CACHE: OnceLock<parking_lot::Mutex<HashMap<String, (u64, u64, String)>>> =
    OnceLock::new();

/// Strict project-id gate. Project ids are lowercase hyphenated UUIDs (see
/// `ml_studio::repository::create_project`). Because the id is interpolated into
/// the cache filename, this is also the first line of defence against path
/// traversal: no `/`, `..` or NUL can survive it.
pub fn validate_project_id(project_id: &str) -> bool {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
        )
        .expect("ml studio project id regex compiles")
    });
    re.is_match(project_id)
}

/// Composite resource string signed by the per-archive token, so a manifest
/// token cannot be replayed cross-endpoint as the archive token.
fn archive_ref(project_id: &str) -> String {
    format!("{project_id}/archive")
}

/// Cache path of the on-demand export archive for `project_id`. Only meaningful
/// for an id that already passed `validate_project_id`.
fn share_cache_path(project_id: &str) -> PathBuf {
    ml_studio_share_cache_dir().join(format!("{project_id}.zip"))
}

/// Filename offered to the browser/downloader in `Content-Disposition`.
pub fn archive_download_filename(project_id: &str) -> String {
    format!("{project_id}.zip")
}

/// Authenticated caller of the `/ml-studio/share/*` endpoints.
pub enum ShareAuth<'a> {
    /// HMAC signed-URL query (`?token=&exp=&ref=`).
    Signed(&'a FrameQuery),
    /// `Authorization: Bearer` API key, already resolved to an ACTIVE key uid by
    /// `model_bundle::resolve_bearer_api_key`. The per-project scope check
    /// happens inside the handlers so both endpoints enforce it identically.
    ApiKey { key_uid: &'a str },
}

impl ShareAuth<'_> {
    fn audit_fields(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Signed(_) => ("signed_url", None),
            Self::ApiKey { key_uid } => ("api_key", Some(key_uid)),
        }
    }
}

/// Mint the per-archive URL bound to the manifest token's absolute expiry.
fn mint_archive_url(
    issuer: &SignedUrlIssuer,
    project_id: &str,
    expiry_unix_ms: u64,
) -> Result<String, SignedUrlError> {
    let signed: SignedUrl = issuer.issue_with_expiry(archive_ref(project_id), expiry_unix_ms)?;
    Ok(format!(
        "/ml-studio/share/{}/archive?{}",
        project_id,
        signed.query_string()
    ))
}

// -----------------------------------------------------------------------------
// Per-project scope gate + audit (mirrors model_bundle)
// -----------------------------------------------------------------------------

/// Per-project scope gate for API-key callers: explicit `allow` on
/// `('ml_studio_export', project_id)` for this key, default-DENY, fail-closed.
fn api_key_project_allowed(pool: &DbPool, project_id: &str, key_uid: &str) -> bool {
    crate::auth::acl::check_v1_access(
        pool,
        ML_STUDIO_SHARE_RESOURCE_TYPE,
        project_id,
        &crate::auth::acl::Principal::ApiKey {
            uid: key_uid.to_string(),
        },
    )
}

/// Caller identity collected for the audit row — HMAC-only callers have no
/// authenticated principal.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestContext<'a> {
    pub source_ip: Option<&'a str>,
    pub user_agent: Option<&'a str>,
}

#[allow(clippy::too_many_arguments)]
fn audit_access(
    pool: &DbPool,
    resource_id: &str,
    ctx: RequestContext<'_>,
    result: &'static str,
    reason: Option<String>,
    severity: &'static str,
    size: Option<i64>,
    auth: &'static str,
    api_key_uid: Option<&str>,
) {
    let details = serde_json::json!({
        "ref": resource_id,
        "size": size,
        "auth": auth,
        "api_key_uid": api_key_uid,
        "source_ip": ctx.source_ip.unwrap_or(""),
        "user_agent": ctx
            .user_agent
            .map(|s| s.chars().take(256).collect::<String>())
            .unwrap_or_default(),
    })
    .to_string();
    if let Ok(conn) = pool.write() {
        let _ = conn.execute(
            "INSERT INTO audit_log \
                (timestamp, user_id, addon_id, action, resource_type, resource_id, \
                 result, error_message, severity, risk_class, details) \
             VALUES (datetime('now'), NULL, NULL, 'ml_studio_share_url_access', \
                     'ml_studio_export', ?1, ?2, ?3, ?4, 'B', ?5)",
            params![resource_id, result, reason, severity, details],
        );
    }
}

// -----------------------------------------------------------------------------
// On-demand build + cache
// -----------------------------------------------------------------------------

fn build_singleflight_gate(project_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    BUILD_SINGLEFLIGHT
        .get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
        .lock()
        .entry(project_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn hash_cache() -> &'static parking_lot::Mutex<HashMap<String, (u64, u64, String)>> {
    HASH_CACHE.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn mtime_unix_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `projects.updated_at` (SQLite `datetime('now')`, UTC "YYYY-MM-DD HH:MM:SS")
/// as a unix timestamp. `None` if the project does not exist — the handlers
/// treat that as a 404.
fn project_updated_at_unix(project_id: &str) -> Option<i64> {
    let pool = crate::ml_studio::db::pool().ok()?;
    let conn = pool.read().ok()?;
    let raw: String = conn
        .query_row(
            "SELECT updated_at FROM projects WHERE project_id = ?1",
            params![project_id],
            |r| r.get(0),
        )
        .ok()?;
    chrono::NaiveDateTime::parse_from_str(&raw, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|dt| dt.and_utc().timestamp())
}

/// A ready cached archive: its path plus the (mtime, size) that key the hash
/// cache and drive the manifest.
struct CachedArchive {
    path: PathBuf,
    mtime: u64,
    size: u64,
}

/// Errors from ensuring an archive exists. `NotFound` is a project that does not
/// exist (or was deleted mid-build); everything else is an internal failure.
enum ArchiveBuildError {
    NotFound,
    Internal,
}

/// Return the cached archive metadata if the file exists AND is fresh: not a
/// symlink, within the staleness TTL, and built AFTER the project's last DB
/// change. `None` forces a rebuild.
async fn cached_if_fresh(path: &Path, project_id: &str) -> Option<CachedArchive> {
    let meta = tokio::fs::symlink_metadata(path).await.ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    let mtime = mtime_unix_secs(&meta);
    let size = meta.len();
    if now_unix_secs().saturating_sub(mtime) > SHARE_CACHE_MAX_AGE_SECS {
        return None;
    }
    match project_updated_at_unix(project_id) {
        Some(updated) if updated > mtime as i64 => None,
        Some(_) => Some(CachedArchive {
            path: path.to_path_buf(),
            mtime,
            size,
        }),
        None => None,
    }
}

/// Build (or reuse a fresh cache of) the project's export archive. Guarded by a
/// per-project singleflight and a global build semaphore so concurrent share
/// requests never spawn many simultaneous multi-gigabyte builds.
async fn ensure_archive(project_id: &str) -> Result<CachedArchive, ArchiveBuildError> {
    let path = share_cache_path(project_id);
    if let Some(cached) = cached_if_fresh(&path, project_id).await {
        return Ok(cached);
    }

    let gate = build_singleflight_gate(project_id);
    let _flight = gate.lock().await;
    // A racing request may have finished the build while we waited on the gate.
    if let Some(cached) = cached_if_fresh(&path, project_id).await {
        return Ok(cached);
    }

    let _permit = BUILD_CONCURRENCY
        .acquire()
        .await
        .expect("BUILD_CONCURRENCY never closed");

    let pid = project_id.to_string();
    let dest = path.clone();
    let built = tokio::task::spawn_blocking(move || {
        build_export(&pid, SHARE_EXPORT_OPTIONS, &dest)
    })
    .await;
    match built {
        Ok(Ok(_summary)) => {}
        Ok(Err(_)) => {
            // The only recoverable cause is a missing project (deleted between
            // the existence pre-check and the build); anything else is internal.
            return Err(if project_updated_at_unix(project_id).is_none() {
                ArchiveBuildError::NotFound
            } else {
                ArchiveBuildError::Internal
            });
        }
        Err(_) => return Err(ArchiveBuildError::Internal),
    }

    let meta = match tokio::fs::metadata(&path).await {
        Ok(m) if m.is_file() => m,
        _ => return Err(ArchiveBuildError::Internal),
    };
    Ok(CachedArchive {
        path,
        mtime: mtime_unix_secs(&meta),
        size: meta.len(),
    })
}

/// Streaming SHA-256 of the cached archive with a per-project (mtime, size)
/// cache. Reuses `model_bundle::sha256_file_hex` (runs on the blocking pool).
async fn cached_archive_sha256(project_id: &str, cached: &CachedArchive) -> std::io::Result<String> {
    if let Some((m, s, hash)) = hash_cache().lock().get(project_id) {
        if *m == cached.mtime && *s == cached.size {
            return Ok(hash.clone());
        }
    }
    let hash = crate::api::model_bundle::sha256_file_hex(&cached.path).await?;
    hash_cache()
        .lock()
        .insert(project_id.to_string(), (cached.mtime, cached.size, hash.clone()));
    Ok(hash)
}

// -----------------------------------------------------------------------------
// Outcomes
// -----------------------------------------------------------------------------

#[derive(Debug)]
pub enum ShareManifestOutcome {
    Ok { body: String },
    BadRequest(&'static str),
    Denied(SignedUrlError),
    /// API-key caller without an `allow` scope on this project.
    Forbidden(&'static str),
    /// Project does not exist.
    NotFound,
    InternalError(&'static str),
}

impl ShareManifestOutcome {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Ok { .. } => 200,
            Self::BadRequest(_) => 400,
            Self::Denied(_) | Self::Forbidden(_) => 403,
            Self::NotFound => 404,
            Self::InternalError(_) => 500,
        }
    }

    fn audit_result(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::BadRequest(_) => "bad_request",
            Self::Denied(_) | Self::Forbidden(_) => "denied",
            Self::NotFound => "not_found",
            Self::InternalError(_) => "error",
        }
    }

    fn audit_reason(&self) -> Option<String> {
        match self {
            Self::Ok { .. } => None,
            Self::BadRequest(why) => Some((*why).to_string()),
            Self::Denied(e) => Some(format!("{e}")),
            Self::Forbidden(why) => Some((*why).to_string()),
            Self::NotFound => Some("project_not_found".to_string()),
            Self::InternalError(why) => Some((*why).to_string()),
        }
    }
}

#[derive(Debug)]
pub enum ShareArchiveOutcome {
    /// Token/scope verified + archive opened (O_NOFOLLOW) + fstat'ed. The HTTP
    /// layer streams the open handle (optionally a byte range). `sha256` is the
    /// FULL-archive digest (already computed/cached by `ensure_archive`), emitted
    /// as `X-Archive-Sha256` so the puller can verify integrity without the
    /// manifest carrying it. With a Range request it still advertises the whole
    /// file's digest, not the partial range.
    Ok {
        file: tokio::fs::File,
        size: u64,
        sha256: String,
    },
    BadRequest(&'static str),
    Denied(SignedUrlError),
    Forbidden(&'static str),
    NotFound,
    /// Symlink or canonical path escaping the share cache dir — tampering.
    PathTraversal,
    IoError,
}

impl ShareArchiveOutcome {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Ok { .. } => 200,
            Self::BadRequest(_) => 400,
            Self::Denied(_) | Self::Forbidden(_) | Self::PathTraversal => 403,
            Self::NotFound => 404,
            Self::IoError => 500,
        }
    }

    fn audit_result(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::BadRequest(_) => "bad_request",
            Self::Denied(_) | Self::Forbidden(_) | Self::PathTraversal => "denied",
            Self::NotFound => "not_found",
            Self::IoError => "error",
        }
    }

    fn audit_reason(&self) -> Option<String> {
        match self {
            Self::Ok { .. } => None,
            Self::BadRequest(why) => Some((*why).to_string()),
            Self::Denied(e) => Some(format!("{e}")),
            Self::Forbidden(why) => Some((*why).to_string()),
            Self::NotFound => Some("project_not_found".to_string()),
            Self::PathTraversal => Some("path_outside_share_cache_dir".to_string()),
            Self::IoError => Some("archive_open_failed".to_string()),
        }
    }

    fn audit_severity(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "info",
            Self::PathTraversal | Self::IoError => "error",
            _ => "warn",
        }
    }
}

// -----------------------------------------------------------------------------
// Handlers
// -----------------------------------------------------------------------------

/// Extract (token, exp) after enforcing that the `ref` query param matches the
/// expected resource — same contract as `/models/*` and `/recordings`.
fn checked_query<'q>(
    query: &'q FrameQuery,
    expected_ref: &str,
) -> Result<(&'q str, u64), &'static str> {
    let token = match query.token.as_deref() {
        Some(t) if !t.is_empty() => t,
        _ => return Err("missing_token"),
    };
    let exp_ms = query.exp_ms.ok_or("missing_exp")?;
    match query.ref_param.as_deref() {
        Some(r) if r == expected_ref => Ok((token, exp_ms)),
        Some(_) => Err("ref_path_mismatch"),
        None => Err("missing_ref"),
    }
}

/// GET /ml-studio/share/<project_id>/manifest — authenticate (signed token OR
/// API-key scope), build/cache the export archive on demand, and emit the
/// project inventory plus a single archive URL. Signed callers get a per-ref
/// signed archive URL bound to the manifest token's expiry; API-key callers get
/// a token-less archive path and repeat the Bearer header on the archive GET.
pub async fn handle_share_manifest(
    project_id: &str,
    auth: &ShareAuth<'_>,
    issuer: &SignedUrlIssuer,
    pool: &DbPool,
    ctx: RequestContext<'_>,
) -> ShareManifestOutcome {
    let outcome = handle_share_manifest_inner(project_id, auth, issuer, pool).await;
    let size = match &outcome {
        ShareManifestOutcome::Ok { body } => Some(body.len() as i64),
        _ => None,
    };
    let severity = match &outcome {
        ShareManifestOutcome::Ok { .. } => "info",
        ShareManifestOutcome::InternalError(_) => "error",
        _ => "warn",
    };
    let (auth_label, key_uid) = auth.audit_fields();
    audit_access(
        pool,
        project_id,
        ctx,
        outcome.audit_result(),
        outcome.audit_reason(),
        severity,
        size,
        auth_label,
        key_uid,
    );
    outcome
}

async fn handle_share_manifest_inner(
    project_id: &str,
    auth: &ShareAuth<'_>,
    issuer: &SignedUrlIssuer,
    pool: &DbPool,
) -> ShareManifestOutcome {
    if !validate_project_id(project_id) {
        return ShareManifestOutcome::BadRequest("invalid_project_id");
    }
    // `Some(exp)` = signed caller (archive URL inherits this expiry);
    // `None` = API-key caller (token-less archive path).
    let archive_url_expiry: Option<u64> = match auth {
        ShareAuth::Signed(query) => {
            let (token, exp_ms) = match checked_query(query, project_id) {
                Ok(v) => v,
                Err(why) => return ShareManifestOutcome::BadRequest(why),
            };
            if let Err(e) = issuer.verify(project_id, exp_ms, token) {
                return ShareManifestOutcome::Denied(e);
            }
            Some(exp_ms)
        }
        ShareAuth::ApiKey { key_uid } => {
            if !api_key_project_allowed(pool, project_id, key_uid) {
                return ShareManifestOutcome::Forbidden("api_key_scope_denied");
            }
            None
        }
    };

    // Cheap project inventory read straight from the DB — NO archive build. This is
    // what keeps the manifest well under the 30 s request-response limit even for
    // multi-gigabyte projects; the archive is built lazily at download time.
    let pid = project_id.to_string();
    let preview =
        match tokio::task::spawn_blocking(move || project_archive::load_project_preview(&pid)).await
        {
            Ok(Ok(Some(p))) => p,
            Ok(Ok(None)) => return ShareManifestOutcome::NotFound,
            _ => return ShareManifestOutcome::InternalError("project_preview_failed"),
        };

    let datasets: Vec<serde_json::Value> = preview
        .datasets
        .iter()
        .map(|d| {
            serde_json::json!({
                "id": d.dataset_id,
                "name": d.name,
                "image_count": d.image_count,
                "annotation_count": d.annotation_count,
            })
        })
        .collect();

    // Archive size + digest: if a FRESH archive is already cached, advertise its real
    // size and cached sha256; otherwise size is an on-disk ESTIMATE (no zip) and the
    // sha256 is omitted (empty) — the authoritative digest is the archive endpoint's
    // `X-Archive-Sha256` header, which the puller verifies against.
    let (archive_size, archive_sha256) = match cached_if_fresh(&share_cache_path(project_id), project_id).await
    {
        Some(cached) => {
            let sha = cached_archive_sha256(project_id, &cached).await.unwrap_or_default();
            (cached.size, sha)
        }
        None => {
            let pid = project_id.to_string();
            let est = tokio::task::spawn_blocking(move || {
                project_archive::estimate_export_size(&pid, SHARE_EXPORT_OPTIONS.include_models)
            })
            .await;
            (est.ok().and_then(|r| r.ok()).unwrap_or(0), String::new())
        }
    };

    // Signed callers get a per-ref signed archive URL bound to the manifest
    // token's expiry; API-key callers carry a plain path and repeat the Bearer
    // header on the archive GET (the archive endpoint re-checks the scope).
    let archive_url = match archive_url_expiry {
        Some(exp_ms) => match mint_archive_url(issuer, project_id, exp_ms) {
            Ok(u) => u,
            Err(SignedUrlError::Expired) => {
                return ShareManifestOutcome::Denied(SignedUrlError::Expired)
            }
            Err(_) => return ShareManifestOutcome::InternalError("mint_archive_url_failed"),
        },
        None => format!("/ml-studio/share/{project_id}/archive"),
    };

    let body = serde_json::json!({
        "project": {
            "id": preview.project_id,
            "name": preview.name,
            "type": preview.project_type,
        },
        "datasets": datasets,
        "classes": preview.classes,
        "archive": {
            "size": archive_size,
            "sha256": archive_sha256,
            "url": archive_url,
        },
        "archive_version": ARCHIVE_VERSION,
    });
    ShareManifestOutcome::Ok {
        body: body.to_string(),
    }
}

/// GET /ml-studio/share/<project_id>/archive — verify the per-archive token (or
/// the API key's project scope), (re)build/cache the archive, contain the path,
/// and hand back the open handle + size for the HTTP layer to stream (with Range
/// support — archives reach ~8 GB). The audit row is written here for every
/// outcome.
pub async fn handle_share_archive(
    project_id: &str,
    auth: &ShareAuth<'_>,
    issuer: &SignedUrlIssuer,
    pool: &DbPool,
    ctx: RequestContext<'_>,
) -> ShareArchiveOutcome {
    let outcome = handle_share_archive_inner(project_id, auth, issuer, pool).await;
    let resource = archive_ref(project_id);
    let size = match &outcome {
        ShareArchiveOutcome::Ok { size, .. } => Some(*size as i64),
        _ => None,
    };
    let (auth_label, key_uid) = auth.audit_fields();
    audit_access(
        pool,
        &resource,
        ctx,
        outcome.audit_result(),
        outcome.audit_reason(),
        outcome.audit_severity(),
        size,
        auth_label,
        key_uid,
    );
    outcome
}

async fn handle_share_archive_inner(
    project_id: &str,
    auth: &ShareAuth<'_>,
    issuer: &SignedUrlIssuer,
    pool: &DbPool,
) -> ShareArchiveOutcome {
    if !validate_project_id(project_id) {
        return ShareArchiveOutcome::BadRequest("invalid_project_id");
    }
    match auth {
        ShareAuth::Signed(query) => {
            let resource = archive_ref(project_id);
            let (token, exp_ms) = match checked_query(query, &resource) {
                Ok(v) => v,
                Err(why) => return ShareArchiveOutcome::BadRequest(why),
            };
            if let Err(e) = issuer.verify(&resource, exp_ms, token) {
                return ShareArchiveOutcome::Denied(e);
            }
        }
        ShareAuth::ApiKey { key_uid } => {
            // Same scope gate as the manifest — a key without the project's
            // allow rule cannot fetch the archive even with a leaked manifest.
            if !api_key_project_allowed(pool, project_id, key_uid) {
                return ShareArchiveOutcome::Forbidden("api_key_scope_denied");
            }
        }
    }

    if project_updated_at_unix(project_id).is_none() {
        return ShareArchiveOutcome::NotFound;
    }

    let cached = match ensure_archive(project_id).await {
        Ok(c) => c,
        Err(ArchiveBuildError::NotFound) => return ShareArchiveOutcome::NotFound,
        Err(ArchiveBuildError::Internal) => return ShareArchiveOutcome::IoError,
    };

    // Authoritative full-archive digest, reused from the hash cache populated by
    // `ensure_archive` (it just hashed a freshly built archive) — never re-hashed
    // here. Emitted as `X-Archive-Sha256` so the puller can verify integrity even
    // though the manifest no longer builds the archive to carry it.
    let sha256 = match cached_archive_sha256(project_id, &cached).await {
        Ok(h) => h,
        Err(_) => return ShareArchiveOutcome::IoError,
    };

    // The cache path is built from a UUID-validated id, so it is contained in
    // the share cache dir by construction. The file is opened here with
    // O_NOFOLLOW and fstat'ed through the handle — validation and serving use
    // the same inode, closing the TOCTOU window a check-then-reopen would leave.
    let path = cached.path;
    #[cfg(not(unix))]
    {
        match tokio::fs::symlink_metadata(&path).await {
            Ok(m) if m.file_type().is_symlink() => return ShareArchiveOutcome::PathTraversal,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ShareArchiveOutcome::NotFound
            }
            Err(_) => return ShareArchiveOutcome::IoError,
            Ok(_) => {}
        }
    }
    let opened = tokio::task::spawn_blocking(move || {
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        opts.open(&path)
    })
    .await;
    let std_file = match opened {
        Ok(Ok(f)) => f,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return ShareArchiveOutcome::NotFound
        }
        #[cfg(unix)]
        Ok(Err(e)) if e.raw_os_error() == Some(libc::ELOOP) => {
            return ShareArchiveOutcome::PathTraversal
        }
        Ok(Err(_)) => return ShareArchiveOutcome::IoError,
        Err(_) => return ShareArchiveOutcome::IoError,
    };
    let meta = match std_file.metadata() {
        Ok(m) => m,
        Err(_) => return ShareArchiveOutcome::IoError,
    };
    if !meta.file_type().is_file() {
        return ShareArchiveOutcome::NotFound;
    }
    ShareArchiveOutcome::Ok {
        file: tokio::fs::File::from_std(std_file),
        size: meta.len(),
        sha256,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::signed_urls::UrlScope;

    const PROJ_A: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn issuer() -> SignedUrlIssuer {
        SignedUrlIssuer::new_for_tests(UrlScope::MlStudioExport, [0x5Au8; 32])
    }

    fn query_from_url(url: &str) -> FrameQuery {
        let raw = url.split_once('?').expect("query present").1;
        crate::api::frames::parse_query(raw).expect("parse")
    }

    #[test]
    fn project_id_validation() {
        assert!(validate_project_id(PROJ_A));
        assert!(!validate_project_id(""));
        assert!(!validate_project_id("not-a-uuid"));
        assert!(!validate_project_id("../../etc/passwd"));
        assert!(!validate_project_id(&format!("{PROJ_A}/../../etc/passwd")));
        // Uppercase is not how ids are generated and must not pass.
        assert!(!validate_project_id("550E8400-E29B-41D4-A716-446655440000"));
    }

    #[test]
    fn archive_token_binds_composite_and_rejects_manifest_replay() {
        let iss = issuer();
        // Manifest token over the bare project id.
        let manifest = iss.issue(PROJ_A.to_string(), 3600).expect("issue");
        // Archive URL derived from the manifest token's expiry, signing the
        // composite `<project_id>/archive`.
        let url = mint_archive_url(&iss, PROJ_A, manifest.expiry_unix_ms).expect("mint archive url");
        assert!(url.starts_with(&format!("/ml-studio/share/{PROJ_A}/archive?")));
        let q = query_from_url(&url);
        let composite = archive_ref(PROJ_A);
        let (token, exp) = checked_query(&q, &composite).expect("query ok");
        assert_eq!(exp, manifest.expiry_unix_ms);
        iss.verify(&composite, exp, token).expect("verify archive token");

        // The manifest token itself must NOT verify as the archive token.
        assert_eq!(
            iss.verify(&composite, manifest.expiry_unix_ms, &manifest.token_b64)
                .unwrap_err(),
            SignedUrlError::InvalidSignature
        );
    }

    #[test]
    fn manifest_outcome_status_codes() {
        assert_eq!(ShareManifestOutcome::Ok { body: String::new() }.http_status(), 200);
        assert_eq!(ShareManifestOutcome::BadRequest("x").http_status(), 400);
        assert_eq!(ShareManifestOutcome::Forbidden("x").http_status(), 403);
        assert_eq!(
            ShareManifestOutcome::Denied(SignedUrlError::Expired).http_status(),
            403
        );
        assert_eq!(ShareManifestOutcome::NotFound.http_status(), 404);
        assert_eq!(ShareManifestOutcome::InternalError("x").http_status(), 500);
    }

    #[test]
    fn archive_outcome_status_codes() {
        assert_eq!(ShareArchiveOutcome::BadRequest("x").http_status(), 400);
        assert_eq!(ShareArchiveOutcome::Forbidden("x").http_status(), 403);
        assert_eq!(ShareArchiveOutcome::PathTraversal.http_status(), 403);
        assert_eq!(ShareArchiveOutcome::NotFound.http_status(), 404);
        assert_eq!(ShareArchiveOutcome::IoError.http_status(), 500);
    }

    // ---- API-key (Bearer) scope gate ---------------------------------------

    fn fresh_db() -> DbPool {
        crate::db::init(std::path::Path::new(":memory:")).expect("init test DB")
    }

    fn test_cipher() -> crate::crypto::SettingsCipher {
        crate::crypto::SettingsCipher::new(&[7u8; 32])
    }

    fn make_key(
        db: &DbPool,
        cipher: &crate::crypto::SettingsCipher,
        scope_project: Option<&str>,
    ) -> (String, String) {
        let pepper = crate::db::repository::get_or_create_api_key_pepper(db, cipher).unwrap();
        let raw = "sk-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let verifier = crate::api::dashboard::auth::api_key_verifier(raw, &pepper);
        let scopes: Vec<(String, String)> = scope_project
            .map(|p| vec![(ML_STUDIO_SHARE_RESOURCE_TYPE.to_string(), p.to_string())])
            .unwrap_or_default();
        let (_id, uid) = crate::db::repository::create_api_key_with_scopes(
            db, &verifier, "sk-...cdef", "share-key", "general", None, 60, &scopes, None, None,
        )
        .expect("create key");
        (raw.to_string(), uid)
    }

    #[test]
    fn api_key_scope_gate_allows_only_scoped_project() {
        let db = fresh_db();
        let cipher = test_cipher();
        let (_t, uid) = make_key(&db, &cipher, Some(PROJ_A));
        assert!(api_key_project_allowed(&db, PROJ_A, &uid));
        assert!(!api_key_project_allowed(
            &db,
            "11111111-2222-3333-4444-555555555555",
            &uid
        ));
    }
}
