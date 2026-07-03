// =============================================================================
// File: api/model_bundle.rs — GET /models/manifest/<bundle_ref> +
//       GET /models/file/<bundle_ref>/<name> signed-URL handlers
// =============================================================================
//
// HTTPS distribution of vision model bundles between TentaFlow instances.
// An admin on the serving node mints a manifest URL (scope
// `UrlScope::ModelBundle`); the pulling node fetches the manifest JSON and
// then downloads each file through a per-file signed URL derived from the
// manifest token's remaining lifetime. Per-file tokens sign the composite
// `<bundle_ref>/<name>` resource so a manifest token cannot be replayed as an
// arbitrary file token and a file token for one file cannot fetch another.
//
// Files are served straight out of `paths::vision_models_dir()` behind a
// strict filename allowlist (no dotfiles, no subdirectories, extension gate)
// plus symlink rejection and canonical-path containment. Bodies are streamed
// in chunks — bundle weights reach ~126 MB, far past the buffered-Vec pattern
// used by `/recordings`.
//
// Every manifest/file fetch writes one `audit_log` row with
// `action='model_bundle_url_access'`, mirroring `api/recording.rs`. Tokens
// are never logged.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use hyper::body::{Bytes, Frame};
use rusqlite::params;
use sha2::{Digest, Sha256};

use crate::api::frames::FrameQuery;
use crate::db::DbPool;
use crate::paths::vision_models_dir;
use crate::services::signed_urls::{SignedUrl, SignedUrlError, SignedUrlIssuer};

/// Chunk size for the streaming file body. 256 KiB keeps per-request memory
/// flat while staying large enough that syscall overhead is negligible.
const STREAM_CHUNK_BYTES: usize = 256 * 1024;

/// Hard cap on manifest-endpoint fan-out. `vision_models_dir()` is a curated
/// deploy target, not user storage — more entries than this means something
/// is wrong and hashing them all per request would be a DoS lever.
const MAX_MANIFEST_FILES: usize = 256;

/// Hard cap on directory entries EXAMINED during the `vision-all` scan —
/// bounds the walk itself, not just the accepted names, so a directory
/// stuffed with allowlist-failing junk cannot stretch the request.
const MAX_SCAN_ENTRIES: usize = 2048;

/// Pseudo bundle_ref exposing every allowlisted file in `vision_models_dir()`.
pub const BUNDLE_REF_ALL: &str = "vision-all";

/// Default TTL when the (future) dashboard handler mints a manifest link —
/// 24 h fits "generate a link, pull it from the other site later today".
pub const DEFAULT_MODEL_BUNDLE_TTL_SECS: u64 = 24 * 3600;

/// Extensions servable through the bundle endpoints. `.onnx.data` is covered
/// by the `.data` suffix combined with the full-name allowlist below.
const ALLOWED_SUFFIXES: &[&str] = &[".onnx", ".onnx.data", ".bpk", ".json", ".txt"];

/// Strict filename gate: `[A-Za-z0-9._-]+`, must not start with a dot (no
/// dotfiles, no `.`/`..`), no path separators, and the extension must be one
/// of the model-bundle types. Anything else never reaches the filesystem.
pub fn validate_file_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 200 {
        return false;
    }
    let bytes = name.as_bytes();
    if bytes[0] == b'.' {
        return false;
    }
    if !bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return false;
    }
    if name.contains("..") {
        return false;
    }
    ALLOWED_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// A bundle_ref is either a camera-CV engine id from `BUNDLES` or the literal
/// `vision-all` (everything on disk that passes the allowlist).
pub fn validate_bundle_ref(bundle_ref: &str) -> bool {
    bundle_ref == BUNDLE_REF_ALL
        || crate::vision::camera_cv_models::is_camera_cv_engine(bundle_ref)
}

/// Composite resource string signed by per-file tokens.
fn file_ref(bundle_ref: &str, name: &str) -> String {
    format!("{}/{}", bundle_ref, name)
}

/// Mint a manifest URL for `bundle_ref`. Entry point for the dashboard link
/// generator; returns the relative signed path (the caller prepends its own
/// externally-reachable origin).
pub fn mint_model_bundle_url(
    issuer: &SignedUrlIssuer,
    bundle_ref: &str,
    ttl_secs: u64,
) -> Result<String, SignedUrlError> {
    if !validate_bundle_ref(bundle_ref) {
        return Err(SignedUrlError::RefInvalid);
    }
    let signed = issuer.issue(bundle_ref.to_string(), ttl_secs)?;
    Ok(format!(
        "/models/manifest/{}?{}",
        bundle_ref,
        signed.query_string()
    ))
}

/// Mint a per-file URL bound to the manifest token's absolute expiry.
fn mint_file_url(
    issuer: &SignedUrlIssuer,
    bundle_ref: &str,
    name: &str,
    expiry_unix_ms: u64,
) -> Result<String, SignedUrlError> {
    let signed: SignedUrl = issuer.issue_with_expiry(file_ref(bundle_ref, name), expiry_unix_ms)?;
    Ok(format!(
        "/models/file/{}/{}?{}",
        bundle_ref,
        name,
        signed.query_string()
    ))
}

/// Verify a per-file token presented at `/models/file/<bundle_ref>/<name>`.
pub fn verify_model_bundle_file_token(
    issuer: &SignedUrlIssuer,
    bundle_ref: &str,
    name: &str,
    expiry_unix_ms: u64,
    token_b64: &str,
) -> Result<(), SignedUrlError> {
    issuer.verify(&file_ref(bundle_ref, name), expiry_unix_ms, token_b64)
}

// -----------------------------------------------------------------------------
// SHA-256 with an in-memory (name, mtime, size) cache
// -----------------------------------------------------------------------------

/// Streaming SHA-256 of a file, hex-encoded. Runs on the blocking pool — the
/// biggest bundle file is ~126 MB and must not stall the async runtime.
pub async fn sha256_file_hex(path: &Path) -> std::io::Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut file = std::fs::File::open(&path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(hex::encode(hasher.finalize()))
    })
    .await
    .map_err(|e| std::io::Error::other(format!("sha256 task join: {e}")))?
}

/// Hash cache keyed by file name — a hit requires the on-disk (mtime, size)
/// pair to match, so a re-deployed weight file is re-hashed exactly once.
static HASH_CACHE: OnceLock<parking_lot::Mutex<HashMap<String, (u64, u64, String)>>> =
    OnceLock::new();

fn hash_cache() -> &'static parking_lot::Mutex<HashMap<String, (u64, u64, String)>> {
    HASH_CACHE.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

/// Per-file singleflight gates: concurrent first requests for the same file
/// wait for one hasher instead of each hashing the 126 MB weights again.
/// Bounded by the file-name allowlist (a few hundred entries max), so the
/// map never grows past the directory's content.
static HASH_SINGLEFLIGHT: OnceLock<
    parking_lot::Mutex<HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
> = OnceLock::new();

/// Global bound on concurrent hashing work across DIFFERENT files — two
/// blocking-pool hashers at a time keeps a cold-cache manifest burst from
/// saturating disk + CPU.
static HASH_CONCURRENCY: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);

fn singleflight_gate(name: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    HASH_SINGLEFLIGHT
        .get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
        .lock()
        .entry(name.to_string())
        .or_insert_with(|| std::sync::Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn mtime_unix_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

async fn cached_sha256(name: &str, path: &Path, meta: &std::fs::Metadata) -> std::io::Result<String> {
    let mtime = mtime_unix_secs(meta);
    let size = meta.len();
    if let Some((m, s, hash)) = hash_cache().lock().get(name) {
        if *m == mtime && *s == size {
            return Ok(hash.clone());
        }
    }
    // Singleflight: losers of the race block here and hit the cache below.
    let gate = singleflight_gate(name);
    let _flight = gate.lock().await;
    if let Some((m, s, hash)) = hash_cache().lock().get(name) {
        if *m == mtime && *s == size {
            return Ok(hash.clone());
        }
    }
    let _permit = HASH_CONCURRENCY
        .acquire()
        .await
        .expect("HASH_CONCURRENCY never closed");
    let hash = sha256_file_hex(path).await?;
    hash_cache()
        .lock()
        .insert(name.to_string(), (mtime, size, hash.clone()));
    Ok(hash)
}

// -----------------------------------------------------------------------------
// Outcomes + audit
// -----------------------------------------------------------------------------

/// Caller identity collected for the audit row — HMAC-only endpoints have no
/// authenticated principal.
#[derive(Debug, Clone, Copy, Default)]
pub struct RequestContext<'a> {
    pub source_ip: Option<&'a str>,
    pub user_agent: Option<&'a str>,
}

#[derive(Debug)]
pub enum ManifestOutcome {
    /// Serialized manifest JSON, ready for a 200 response.
    Ok { body: String },
    BadRequest(&'static str),
    Denied(SignedUrlError),
    /// Bundle known but zero servable files exist on disk.
    NotFound,
    InternalError(&'static str),
}

impl ManifestOutcome {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Ok { .. } => 200,
            Self::BadRequest(_) => 400,
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
            Self::NotFound => Some("no_files_on_disk".to_string()),
            Self::InternalError(why) => Some((*why).to_string()),
        }
    }
}

#[derive(Debug)]
pub enum FileOutcome {
    /// Token verified + file opened (O_NOFOLLOW) + fstat'ed. The HTTP layer
    /// streams the already-open handle — no path re-open, no TOCTOU window.
    Ok { file: tokio::fs::File, size: u64 },
    BadRequest(&'static str),
    Denied(SignedUrlError),
    NotFound,
    /// Symlink or canonical path escaping `vision_models_dir()` — tampering.
    PathTraversal,
    IoError,
}

impl FileOutcome {
    pub fn http_status(&self) -> u16 {
        match self {
            Self::Ok { .. } => 200,
            Self::BadRequest(_) => 400,
            Self::Denied(_) => 403,
            Self::NotFound => 404,
            Self::PathTraversal => 403,
            Self::IoError => 500,
        }
    }

    fn audit_result(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "ok",
            Self::BadRequest(_) => "bad_request",
            Self::Denied(_) | Self::PathTraversal => "denied",
            Self::NotFound => "not_found",
            Self::IoError => "error",
        }
    }

    fn audit_reason(&self) -> Option<String> {
        match self {
            Self::Ok { .. } => None,
            Self::BadRequest(why) => Some((*why).to_string()),
            Self::Denied(e) => Some(format!("{e}")),
            Self::NotFound => Some("file_missing_on_disk".to_string()),
            Self::PathTraversal => Some("path_outside_vision_models_dir".to_string()),
            Self::IoError => Some("file_stat_failed".to_string()),
        }
    }

    fn audit_severity(&self) -> &'static str {
        match self {
            Self::Ok { .. } => "info",
            Self::PathTraversal => "error",
            Self::IoError => "error",
            _ => "warn",
        }
    }
}

fn audit_access(
    pool: &DbPool,
    resource_id: &str,
    ctx: RequestContext<'_>,
    result: &'static str,
    reason: Option<String>,
    severity: &'static str,
    size: Option<i64>,
) {
    let details = serde_json::json!({
        "ref": resource_id,
        "size": size,
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
             VALUES (datetime('now'), NULL, NULL, 'model_bundle_url_access', \
                     'model_bundle', ?1, ?2, ?3, ?4, 'B', ?5)",
            params![resource_id, result, reason, severity, details],
        );
    }
}

// -----------------------------------------------------------------------------
// Handlers
// -----------------------------------------------------------------------------

/// Extract (token, exp) after enforcing that the `ref` query param matches
/// the path-derived resource — same contract as `/recordings`.
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

/// GET /models/manifest/<bundle_ref> — verify the manifest token, enumerate
/// the bundle's on-disk files, hash them (cached) and mint one per-file URL
/// per entry with the manifest token's remaining lifetime.
pub async fn handle_manifest(
    bundle_ref: &str,
    query: &FrameQuery,
    issuer: &SignedUrlIssuer,
    pool: &DbPool,
    ctx: RequestContext<'_>,
) -> ManifestOutcome {
    let outcome = handle_manifest_inner(bundle_ref, query, issuer).await;
    let size = match &outcome {
        ManifestOutcome::Ok { body } => Some(body.len() as i64),
        _ => None,
    };
    let severity = match &outcome {
        ManifestOutcome::Ok { .. } => "info",
        ManifestOutcome::InternalError(_) => "error",
        _ => "warn",
    };
    audit_access(
        pool,
        bundle_ref,
        ctx,
        outcome.audit_result(),
        outcome.audit_reason(),
        severity,
        size,
    );
    outcome
}

async fn handle_manifest_inner(
    bundle_ref: &str,
    query: &FrameQuery,
    issuer: &SignedUrlIssuer,
) -> ManifestOutcome {
    if !validate_bundle_ref(bundle_ref) {
        return ManifestOutcome::BadRequest("invalid_bundle_ref");
    }
    let (token, exp_ms) = match checked_query(query, bundle_ref) {
        Ok(v) => v,
        Err(why) => return ManifestOutcome::BadRequest(why),
    };
    if let Err(e) = issuer.verify(bundle_ref, exp_ms, token) {
        return ManifestOutcome::Denied(e);
    }

    let names = match list_bundle_files(bundle_ref) {
        Ok(n) => n,
        Err(()) => return ManifestOutcome::InternalError("list_dir_failed"),
    };
    if names.is_empty() {
        return ManifestOutcome::NotFound;
    }

    let dir = vision_models_dir();
    let mut files = Vec::with_capacity(names.len());
    for name in names {
        let path = dir.join(&name);
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() => m,
            // Symlinks/dirs are silently excluded — the manifest only ever
            // advertises regular files the file endpoint would actually serve.
            Ok(_) => continue,
            Err(_) => continue,
        };
        let sha256 = match cached_sha256(&name, &path, &meta).await {
            Ok(h) => h,
            Err(_) => return ManifestOutcome::InternalError("hash_failed"),
        };
        // Per-file token inherits the manifest token's absolute expiry, so a
        // stashed manifest response cannot outlive the link the admin minted.
        let url = match mint_file_url(issuer, bundle_ref, &name, exp_ms) {
            Ok(u) => u,
            Err(SignedUrlError::Expired) => {
                return ManifestOutcome::Denied(SignedUrlError::Expired)
            }
            Err(_) => return ManifestOutcome::InternalError("mint_file_url_failed"),
        };
        files.push(serde_json::json!({
            "name": name,
            "size": meta.len(),
            "sha256": sha256,
            "url": url,
        }));
    }
    if files.is_empty() {
        return ManifestOutcome::NotFound;
    }

    let body = serde_json::json!({
        "bundle": bundle_ref,
        "files": files,
    })
    .to_string();
    ManifestOutcome::Ok { body }
}

/// Resolve the servable file names for a bundle_ref. Named bundles use the
/// static `BUNDLES` file list (filtered by the allowlist for defense in
/// depth); `vision-all` scans the directory.
fn list_bundle_files(bundle_ref: &str) -> Result<Vec<String>, ()> {
    if bundle_ref != BUNDLE_REF_ALL {
        let names = crate::vision::camera_cv_models::bundle_file_names(bundle_ref).ok_or(())?;
        return Ok(names
            .into_iter()
            .filter(|n| validate_file_name(n))
            .map(str::to_string)
            .collect());
    }
    let dir = vision_models_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        // A node that never deployed vision models has no directory — that is
        // "no files", not an internal error.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(()),
    };
    let mut names: Vec<String> = entries
        .take(MAX_SCAN_ENTRIES)
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| validate_file_name(n))
        .take(MAX_MANIFEST_FILES)
        .collect();
    names.sort();
    Ok(names)
}

/// GET /models/file/<bundle_ref>/<name> — verify the per-file token, contain
/// the path, and hand back the file location + size for the HTTP layer to
/// stream. The audit row is written here for every outcome; streaming errors
/// after the 200 status has been sent surface as a truncated body (the
/// client's sha256 check from the manifest catches them).
pub async fn handle_file(
    bundle_ref: &str,
    name: &str,
    query: &FrameQuery,
    issuer: &SignedUrlIssuer,
    pool: &DbPool,
    ctx: RequestContext<'_>,
) -> FileOutcome {
    let outcome = handle_file_inner(bundle_ref, name, query, issuer).await;
    let resource = file_ref(bundle_ref, name);
    let size = match &outcome {
        FileOutcome::Ok { size, .. } => Some(*size as i64),
        _ => None,
    };
    audit_access(
        pool,
        &resource,
        ctx,
        outcome.audit_result(),
        outcome.audit_reason(),
        outcome.audit_severity(),
        size,
    );
    outcome
}

async fn handle_file_inner(
    bundle_ref: &str,
    name: &str,
    query: &FrameQuery,
    issuer: &SignedUrlIssuer,
) -> FileOutcome {
    if !validate_bundle_ref(bundle_ref) {
        return FileOutcome::BadRequest("invalid_bundle_ref");
    }
    if !validate_file_name(name) {
        return FileOutcome::BadRequest("invalid_file_name");
    }
    let resource = file_ref(bundle_ref, name);
    let (token, exp_ms) = match checked_query(query, &resource) {
        Ok(v) => v,
        Err(why) => return FileOutcome::BadRequest(why),
    };
    if let Err(e) = issuer.verify(&resource, exp_ms, token) {
        return FileOutcome::Denied(e);
    }
    // Named-bundle tokens may only fetch files that belong to that bundle;
    // `vision-all` tokens may fetch anything passing the allowlist.
    if bundle_ref != BUNDLE_REF_ALL {
        match crate::vision::camera_cv_models::bundle_file_names(bundle_ref) {
            Some(names) if names.contains(&name) => {}
            _ => return FileOutcome::BadRequest("file_not_in_bundle"),
        }
    }

    // The validated name cannot traverse (no separators, no `..`), so the
    // joined path is contained in `vision_models_dir()` by construction. The
    // file is opened here with O_NOFOLLOW and fstat'ed through the handle —
    // validation and serving use the same inode, closing the TOCTOU window a
    // check-then-reopen sequence would leave.
    let path = vision_models_dir().join(name);
    #[cfg(not(unix))]
    {
        // No O_NOFOLLOW off unix — a pre-open symlink probe is the best
        // available filter there.
        match tokio::fs::symlink_metadata(&path).await {
            Ok(m) if m.file_type().is_symlink() => return FileOutcome::PathTraversal,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return FileOutcome::NotFound,
            Err(_) => return FileOutcome::IoError,
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
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => return FileOutcome::NotFound,
        // O_NOFOLLOW turns a symlink final component into ELOOP.
        #[cfg(unix)]
        Ok(Err(e)) if e.raw_os_error() == Some(libc::ELOOP) => return FileOutcome::PathTraversal,
        Ok(Err(_)) => return FileOutcome::IoError,
        Err(_) => return FileOutcome::IoError,
    };
    // fstat the OPEN handle — metadata of what will actually be streamed.
    let meta = match std_file.metadata() {
        Ok(m) => m,
        Err(_) => return FileOutcome::IoError,
    };
    if !meta.file_type().is_file() {
        return FileOutcome::NotFound;
    }
    FileOutcome::Ok {
        file: tokio::fs::File::from_std(std_file),
        size: meta.len(),
    }
}

/// Chunked body stream over an open file. Fits the dashboard server's
/// `StreamBody<SseStream>` slot (the same mechanism SSE uses), so the 126 MB
/// weights never materialize in memory.
pub fn file_stream(
    file: tokio::fs::File,
) -> impl futures::Stream<Item = Result<Frame<Bytes>, std::io::Error>> + Send {
    futures::stream::unfold(Some(file), |state| async move {
        let mut file = state?;
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; STREAM_CHUNK_BYTES];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Frame::data(Bytes::from(buf))), Some(file)))
            }
            Err(e) => Some((Err(e), None)),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::signed_urls::UrlScope;

    fn issuer() -> SignedUrlIssuer {
        SignedUrlIssuer::new_for_tests(UrlScope::ModelBundle, [0x5Au8; 32])
    }

    fn query_from_url(url: &str) -> FrameQuery {
        let raw = url.split_once('?').expect("query present").1;
        crate::api::frames::parse_query(raw).expect("parse")
    }

    #[test]
    fn mint_and_verify_manifest_token() {
        let iss = issuer();
        let url = mint_model_bundle_url(&iss, BUNDLE_REF_ALL, 3600).expect("mint");
        assert!(url.starts_with("/models/manifest/vision-all?"));
        let q = query_from_url(&url);
        let (token, exp) = checked_query(&q, BUNDLE_REF_ALL).expect("query ok");
        iss.verify(BUNDLE_REF_ALL, exp, token).expect("verify");
    }

    #[test]
    fn mint_rejects_unknown_bundle_ref() {
        let iss = issuer();
        assert_eq!(
            mint_model_bundle_url(&iss, "not-a-bundle", 3600).unwrap_err(),
            SignedUrlError::RefInvalid
        );
    }

    #[test]
    fn manifest_token_ttl_bounds_enforced() {
        let iss = issuer();
        assert!(matches!(
            mint_model_bundle_url(&iss, BUNDLE_REF_ALL, 60).unwrap_err(),
            SignedUrlError::TtlOutOfRange(60, 300, 604800)
        ));
        assert!(matches!(
            mint_model_bundle_url(&iss, BUNDLE_REF_ALL, 8 * 24 * 3600).unwrap_err(),
            SignedUrlError::TtlOutOfRange(_, 300, 604800)
        ));
    }

    #[test]
    fn per_file_token_derives_and_rejects_cross_file_replay() {
        let iss = issuer();
        let manifest = iss.issue(BUNDLE_REF_ALL.to_string(), 3600).expect("issue");
        let url = mint_file_url(&iss, BUNDLE_REF_ALL, "rfdetr-base.bpk", manifest.expiry_unix_ms)
            .expect("mint file url");
        assert!(url.starts_with("/models/file/vision-all/rfdetr-base.bpk?"));
        let q = query_from_url(&url);
        let expected_ref = "vision-all/rfdetr-base.bpk";
        let (token, exp) = checked_query(&q, expected_ref).expect("query ok");
        assert_eq!(exp, manifest.expiry_unix_ms);
        verify_model_bundle_file_token(&iss, BUNDLE_REF_ALL, "rfdetr-base.bpk", exp, token)
            .expect("verify file token");

        // Same token presented for a DIFFERENT file must fail — the composite
        // ref is part of the HMAC payload.
        assert_eq!(
            verify_model_bundle_file_token(&iss, BUNDLE_REF_ALL, "model_stan.bpk", exp, token)
                .unwrap_err(),
            SignedUrlError::InvalidSignature
        );
        // The manifest token itself must not verify as a file token.
        assert_eq!(
            verify_model_bundle_file_token(
                &iss,
                BUNDLE_REF_ALL,
                "rfdetr-base.bpk",
                manifest.expiry_unix_ms,
                &manifest.token_b64
            )
            .unwrap_err(),
            SignedUrlError::InvalidSignature
        );
    }

    #[test]
    fn issue_with_expiry_rejects_past_and_overlong() {
        let iss = issuer();
        assert_eq!(
            iss.issue_with_expiry("x/y".into(), 1).unwrap_err(),
            SignedUrlError::Expired
        );
        let far = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
            + 30 * 24 * 3600 * 1000;
        assert!(matches!(
            iss.issue_with_expiry("x/y".into(), far).unwrap_err(),
            SignedUrlError::TtlOutOfRange(..)
        ));
    }

    #[test]
    fn file_name_allowlist() {
        assert!(validate_file_name("rfdetr-base.bpk"));
        assert!(validate_file_name("model.onnx"));
        assert!(validate_file_name("model.onnx.data"));
        assert!(validate_file_name("rfdetr-classes.json"));
        assert!(validate_file_name("ppocrv5_dict.txt"));

        assert!(!validate_file_name(""));
        assert!(!validate_file_name(".hidden.json"));
        assert!(!validate_file_name("../etc/passwd"));
        assert!(!validate_file_name("sub/dir.onnx"));
        assert!(!validate_file_name("model..onnx"));
        assert!(!validate_file_name("model.exe"));
        assert!(!validate_file_name("model.gguf"));
        assert!(!validate_file_name("name with space.onnx"));
        assert!(!validate_file_name(&format!("{}.onnx", "a".repeat(300))));
    }

    #[test]
    fn bundle_ref_validation() {
        assert!(validate_bundle_ref("vision-all"));
        assert!(validate_bundle_ref("rfdetr-adr"));
        assert!(validate_bundle_ref("depth-native"));
        assert!(!validate_bundle_ref("llama-cpp"));
        assert!(!validate_bundle_ref("../vision"));
    }
}
