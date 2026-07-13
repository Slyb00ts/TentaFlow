// =============================================================================
// File: api/model_bundle.rs — GET /models/manifest/<bundle_ref> +
//       GET /models/file/<bundle_ref>/<name> signed-URL handlers
// =============================================================================
//
// HTTPS distribution of vision model bundles between TentaFlow instances.
// Two independent auth modes:
//
// 1. Signed URLs (paired/ad-hoc sharing): an admin on the serving node mints
//    a manifest URL (scope `UrlScope::ModelBundle`); the pulling node fetches
//    the manifest JSON and then downloads each file through a per-file signed
//    URL derived from the manifest token's remaining lifetime. Per-file tokens
//    sign the composite `<bundle_ref>/<name>` resource so a manifest token
//    cannot be replayed as an arbitrary file token and a file token for one
//    file cannot fetch another.
//
// 2. API keys (UNPAIRED instances): `Authorization: Bearer <key>` with the
//    same verifier pipeline as `/v1` (pepper + HMAC, fail-closed) plus an
//    explicit `resource_permissions` allow rule on
//    `('model_bundle', <bundle_ref>)` for the key (default-DENY — only
//    'general' keys carry such scopes). API-key manifests deliberately return
//    per-file urls WITHOUT tokens: the client repeats the same Bearer header
//    on each `/models/file/...` GET, which keeps the flow stateless (no
//    signed-URL minting on behalf of a key) while the file endpoint stays
//    airtight — it re-checks the key's bundle scope on every request.
//    Rejected bearers are audited (key id only, never the key).
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

/// A bundle_ref is one of: the literal `vision-all`, a fixed camera-CV engine
/// id from `BUNDLES`, or a `vision_models` registry model name. Only the
/// STRUCTURE is checked here (no DB) — an admin may scope a key to a model
/// name before that model exists, exactly like `model`/`flow`/`alias` scopes.
/// Registry existence is resolved (and 404'd) at manifest/file build time.
pub fn validate_bundle_ref(bundle_ref: &str) -> bool {
    bundle_ref == BUNDLE_REF_ALL
        || crate::vision::camera_cv_models::is_camera_cv_engine(bundle_ref)
        || crate::db::repository::validate_vision_model_name(bundle_ref).is_ok()
}

/// Composite resource string signed by per-file tokens.
fn file_ref(bundle_ref: &str, name: &str) -> String {
    format!("{}/{}", bundle_ref, name)
}

/// ACL resource type carrying API-key bundle scopes in `resource_permissions`.
pub const MODEL_BUNDLE_RESOURCE_TYPE: &str = "model_bundle";

/// Authenticated caller of the `/models/*` endpoints.
pub enum BundleAuth<'a> {
    /// HMAC signed-URL query (`?token=&exp=&ref=`).
    Signed(&'a FrameQuery),
    /// `Authorization: Bearer` API key, already resolved to an ACTIVE key uid
    /// by `resolve_bearer_api_key`. The per-bundle scope check happens inside
    /// the handlers so both endpoints enforce it identically.
    ApiKey { key_uid: &'a str },
}

impl BundleAuth<'_> {
    fn audit_fields(&self) -> (&'static str, Option<&str>) {
        match self {
            Self::Signed(_) => ("signed_url", None),
            Self::ApiKey { key_uid } => ("api_key", Some(key_uid)),
        }
    }
}

/// Result of resolving an `Authorization: Bearer` header on `/models/*`.
pub enum BearerAuthResult {
    Ok(crate::db::models::DbApiKey),
    /// Unknown, inactive or malformed key.
    Invalid,
    /// Pepper unavailable — verification MUST fail closed (same posture as
    /// the /v1 gate: an empty pepper would derive the HMAC under the wrong
    /// key and could match a forged row).
    Unavailable,
}

/// Resolve a Bearer token to an active API key using the SAME verifier
/// pipeline as `/v1` (org pepper + HMAC-SHA256 verifier lookup).
pub fn resolve_bearer_api_key(
    pool: &DbPool,
    cipher: &crate::crypto::SettingsCipher,
    token: &str,
) -> BearerAuthResult {
    let pepper = match crate::db::repository::get_or_create_api_key_pepper(pool, cipher) {
        Ok(p) => p,
        Err(_) => return BearerAuthResult::Unavailable,
    };
    let verifier = crate::api::dashboard::auth::api_key_verifier(token, &pepper);
    match crate::db::repository::verify_api_key(pool, &verifier) {
        Ok(Some(row)) => BearerAuthResult::Ok(row),
        Ok(None) => BearerAuthResult::Invalid,
        Err(_) => BearerAuthResult::Invalid,
    }
}

/// Per-bundle scope gate for API-key callers: explicit `allow` on
/// `('model_bundle', bundle_ref)` for this key, default-DENY, fail-closed.
fn api_key_bundle_allowed(pool: &DbPool, bundle_ref: &str, key_uid: &str) -> bool {
    crate::auth::acl::check_v1_access(
        pool,
        MODEL_BUNDLE_RESOURCE_TYPE,
        bundle_ref,
        &crate::auth::acl::Principal::ApiKey {
            uid: key_uid.to_string(),
        },
    )
}

/// Audit a Bearer rejection that happens before a handler runs (invalid key,
/// verification unavailable, per-key rate limit). The raw key is never logged.
pub fn audit_api_key_rejected(
    pool: &DbPool,
    resource_id: &str,
    ctx: RequestContext<'_>,
    reason: &'static str,
) {
    audit_access(
        pool,
        resource_id,
        ctx,
        "denied",
        Some(reason.to_string()),
        "warn",
        None,
        "api_key",
        None,
    );
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

async fn cached_sha256(
    name: &str,
    path: &Path,
    meta: &std::fs::Metadata,
) -> std::io::Result<String> {
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
    Ok {
        body: String,
    },
    BadRequest(&'static str),
    Denied(SignedUrlError),
    /// API-key caller without an `allow` scope on this bundle.
    Forbidden(&'static str),
    /// Bundle known but zero servable files exist on disk.
    NotFound,
    InternalError(&'static str),
}

impl ManifestOutcome {
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
            Self::NotFound => Some("no_files_on_disk".to_string()),
            Self::InternalError(why) => Some((*why).to_string()),
        }
    }
}

#[derive(Debug)]
pub enum FileOutcome {
    /// Token verified + file opened (O_NOFOLLOW) + fstat'ed. The HTTP layer
    /// streams the already-open handle — no path re-open, no TOCTOU window.
    Ok {
        file: tokio::fs::File,
        size: u64,
    },
    BadRequest(&'static str),
    Denied(SignedUrlError),
    /// API-key caller without an `allow` scope on this bundle.
    Forbidden(&'static str),
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
            Self::Denied(_) | Self::Forbidden(_) => 403,
            Self::NotFound => 404,
            Self::PathTraversal => 403,
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

/// GET /models/manifest/<bundle_ref> — authenticate (signed token OR API key
/// scope), enumerate the bundle's on-disk files, hash them (cached) and emit
/// one per-file URL per entry. Signed callers get per-file signed URLs bound
/// to the manifest token's expiry; API-key callers get token-less paths and
/// repeat the Bearer header on file GETs.
pub async fn handle_manifest(
    bundle_ref: &str,
    auth: &BundleAuth<'_>,
    issuer: &SignedUrlIssuer,
    pool: &DbPool,
    ctx: RequestContext<'_>,
) -> ManifestOutcome {
    let outcome = handle_manifest_inner(bundle_ref, auth, issuer, pool).await;
    let size = match &outcome {
        ManifestOutcome::Ok { body } => Some(body.len() as i64),
        _ => None,
    };
    let severity = match &outcome {
        ManifestOutcome::Ok { .. } => "info",
        ManifestOutcome::InternalError(_) => "error",
        _ => "warn",
    };
    let (auth_label, key_uid) = auth.audit_fields();
    audit_access(
        pool,
        bundle_ref,
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

async fn handle_manifest_inner(
    bundle_ref: &str,
    auth: &BundleAuth<'_>,
    issuer: &SignedUrlIssuer,
    pool: &DbPool,
) -> ManifestOutcome {
    if !validate_bundle_ref(bundle_ref) {
        return ManifestOutcome::BadRequest("invalid_bundle_ref");
    }
    // `Some(exp)` = signed caller (per-file URLs inherit this expiry);
    // `None` = API-key caller (token-less per-file paths).
    let file_url_expiry: Option<u64> = match auth {
        BundleAuth::Signed(query) => {
            let (token, exp_ms) = match checked_query(query, bundle_ref) {
                Ok(v) => v,
                Err(why) => return ManifestOutcome::BadRequest(why),
            };
            if let Err(e) = issuer.verify(bundle_ref, exp_ms, token) {
                return ManifestOutcome::Denied(e);
            }
            Some(exp_ms)
        }
        BundleAuth::ApiKey { key_uid } => {
            if !api_key_bundle_allowed(pool, bundle_ref, key_uid) {
                return ManifestOutcome::Forbidden("api_key_scope_denied");
            }
            None
        }
    };

    let (names, registry_row) = match resolve_bundle(pool, bundle_ref) {
        ResolvedBundle::Fixed(names) => (names, None),
        ResolvedBundle::Registry { files, row } => (files, Some(row)),
        ResolvedBundle::NotFound => return ManifestOutcome::NotFound,
        ResolvedBundle::Error => return ManifestOutcome::InternalError("list_dir_failed"),
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
        // API-key manifests carry plain paths — the client's Bearer header is
        // the credential and the file endpoint re-checks its scope.
        let url = match file_url_expiry {
            Some(exp_ms) => match mint_file_url(issuer, bundle_ref, &name, exp_ms) {
                Ok(u) => u,
                Err(SignedUrlError::Expired) => {
                    return ManifestOutcome::Denied(SignedUrlError::Expired)
                }
                Err(_) => return ManifestOutcome::InternalError("mint_file_url_failed"),
            },
            None => format!("/models/file/{}/{}", bundle_ref, name),
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

    let mut body = serde_json::json!({
        "bundle": bundle_ref,
        "files": files,
    });
    // Registry manifests carry the row metadata so an unpaired client can
    // `register_vision_model` after downloading — a fixed engine bundle never
    // does (its runner is compiled in, nothing to register).
    if let Some(row) = registry_row {
        body["model"] = registry_model_meta(&row);
    }
    ManifestOutcome::Ok {
        body: body.to_string(),
    }
}

/// Resolved bundle: the servable file names plus, for a registry model, its
/// `vision_models` row so the manifest can embed the metadata an unpaired
/// client needs to `register_vision_model` locally.
enum ResolvedBundle {
    /// `vision-all` or a fixed camera-CV engine — file names only.
    Fixed(Vec<String>),
    /// A `vision_models` registry model — its ONNX (+ optional external-data
    /// sibling on disk) and the row.
    Registry {
        files: Vec<String>,
        row: Box<crate::db::repository::VisionModelRow>,
    },
    /// A structurally-valid bundle_ref that resolves to no known bundle.
    NotFound,
    /// DB error while resolving a registry name.
    Error,
}

/// Resolve a bundle_ref to its servable file set. Order: `vision-all` (disk
/// scan) → fixed camera-CV engine (static list) → `vision_models` registry
/// model (single ONNX + optional `.data` sibling). A model name that collides
/// with a fixed engine id resolves to the engine (checked first).
fn resolve_bundle(pool: &DbPool, bundle_ref: &str) -> ResolvedBundle {
    if bundle_ref == BUNDLE_REF_ALL {
        return match scan_all_files() {
            Ok(names) => ResolvedBundle::Fixed(names),
            Err(()) => ResolvedBundle::Error,
        };
    }
    if let Some(names) = crate::vision::camera_cv_models::bundle_file_names(bundle_ref) {
        let files = names
            .into_iter()
            .filter(|n| validate_file_name(n))
            .map(str::to_string)
            .collect();
        return ResolvedBundle::Fixed(files);
    }
    match crate::db::repository::get_vision_model(pool, bundle_ref) {
        Ok(Some(row)) => {
            let mut files = Vec::with_capacity(2);
            if validate_file_name(&row.file_name) {
                files.push(row.file_name.clone());
            }
            // ONNX external-data sibling (`model.onnx.data`) if the exporter
            // emitted one — bounded to this one candidate, allowlist-gated.
            let sibling = format!("{}.data", row.file_name);
            if validate_file_name(&sibling) && vision_models_dir().join(&sibling).is_file() {
                files.push(sibling);
            }
            ResolvedBundle::Registry {
                files,
                row: Box::new(row),
            }
        }
        Ok(None) => ResolvedBundle::NotFound,
        Err(_) => ResolvedBundle::Error,
    }
}

/// Every allowlisted regular file in `vision_models_dir()` (the `vision-all`
/// pseudo-bundle). A missing directory is "no files", not an error.
fn scan_all_files() -> Result<Vec<String>, ()> {
    let dir = vision_models_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
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

/// Metadata object embedded in a registry-model manifest so the pulling node
/// can insert the `vision_models` row without a second round-trip. Mirrors the
/// non-file columns of `VisionModelRow` the importer needs.
fn registry_model_meta(row: &crate::db::repository::VisionModelRow) -> serde_json::Value {
    serde_json::json!({
        "model_name": row.model_name,
        "op": row.op,
        "file_name": row.file_name,
        "classes_json": row.classes_json,
        "preprocess_json": row.preprocess_json,
        "output_contract": row.output_contract,
        "default_threshold": row.default_threshold,
    })
}

/// GET /models/file/<bundle_ref>/<name> — verify the per-file token, contain
/// the path, and hand back the file location + size for the HTTP layer to
/// stream. The audit row is written here for every outcome; streaming errors
/// after the 200 status has been sent surface as a truncated body (the
/// client's sha256 check from the manifest catches them).
pub async fn handle_file(
    bundle_ref: &str,
    name: &str,
    auth: &BundleAuth<'_>,
    issuer: &SignedUrlIssuer,
    pool: &DbPool,
    ctx: RequestContext<'_>,
) -> FileOutcome {
    let outcome = handle_file_inner(bundle_ref, name, auth, issuer, pool).await;
    let resource = file_ref(bundle_ref, name);
    let size = match &outcome {
        FileOutcome::Ok { size, .. } => Some(*size as i64),
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

async fn handle_file_inner(
    bundle_ref: &str,
    name: &str,
    auth: &BundleAuth<'_>,
    issuer: &SignedUrlIssuer,
    pool: &DbPool,
) -> FileOutcome {
    if !validate_bundle_ref(bundle_ref) {
        return FileOutcome::BadRequest("invalid_bundle_ref");
    }
    if !validate_file_name(name) {
        return FileOutcome::BadRequest("invalid_file_name");
    }
    let resource = file_ref(bundle_ref, name);
    match auth {
        BundleAuth::Signed(query) => {
            let (token, exp_ms) = match checked_query(query, &resource) {
                Ok(v) => v,
                Err(why) => return FileOutcome::BadRequest(why),
            };
            if let Err(e) = issuer.verify(&resource, exp_ms, token) {
                return FileOutcome::Denied(e);
            }
        }
        BundleAuth::ApiKey { key_uid } => {
            // Same scope gate as the manifest — a key without the bundle's
            // allow rule cannot fetch files even with a leaked manifest body.
            if !api_key_bundle_allowed(pool, bundle_ref, key_uid) {
                return FileOutcome::Forbidden("api_key_scope_denied");
            }
        }
    }
    // Named-bundle tokens may only fetch files that belong to that bundle;
    // `vision-all` tokens may fetch anything passing the allowlist. Registry
    // bundles are scoped to their ONNX + optional `.data` sibling.
    if bundle_ref != BUNDLE_REF_ALL {
        let allowed = match resolve_bundle(pool, bundle_ref) {
            ResolvedBundle::Fixed(names) => names.iter().any(|n| n == name),
            ResolvedBundle::Registry { files, .. } => files.iter().any(|n| n == name),
            ResolvedBundle::NotFound => return FileOutcome::NotFound,
            ResolvedBundle::Error => return FileOutcome::IoError,
        };
        if !allowed {
            return FileOutcome::BadRequest("file_not_in_bundle");
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
    fn mint_rejects_invalid_bundle_ref() {
        // Structurally invalid refs (registry names are lowercase [a-z0-9-_],
        // so uppercase / separators can never be a bundle_ref).
        let iss = issuer();
        assert_eq!(
            mint_model_bundle_url(&iss, "Not-A-Bundle", 3600).unwrap_err(),
            SignedUrlError::RefInvalid
        );
        assert_eq!(
            mint_model_bundle_url(&iss, "../vision", 3600).unwrap_err(),
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
        let url = mint_file_url(
            &iss,
            BUNDLE_REF_ALL,
            "rfdetr-base.bpk",
            manifest.expiry_unix_ms,
        )
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
        // Any structurally-valid registry model name is a candidate bundle_ref
        // (existence is resolved against the DB at manifest/file build time).
        assert!(validate_bundle_ref("my-trained-detector"));
        assert!(validate_bundle_ref("cysterny_adr_v2"));
        // Structural rejects: separators, dots, uppercase, empty.
        assert!(!validate_bundle_ref("../vision"));
        assert!(!validate_bundle_ref("sub/dir"));
        assert!(!validate_bundle_ref("Upper"));
        assert!(!validate_bundle_ref(""));
    }

    // ---- API-key (Bearer) auth path ----------------------------------------

    fn fresh_db() -> DbPool {
        crate::db::init(std::path::Path::new(":memory:")).expect("init test DB")
    }

    fn test_cipher() -> crate::crypto::SettingsCipher {
        crate::crypto::SettingsCipher::new(&[7u8; 32])
    }

    /// Creates a general API key, optionally scoped to a model_bundle ref, and
    /// returns `(raw_token, uid)`.
    fn make_key(
        db: &DbPool,
        cipher: &crate::crypto::SettingsCipher,
        scope_bundle: Option<&str>,
    ) -> (String, String) {
        let pepper = crate::db::repository::get_or_create_api_key_pepper(db, cipher).unwrap();
        let raw = "sk-0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let verifier = crate::api::dashboard::auth::api_key_verifier(raw, &pepper);
        let scopes: Vec<(String, String)> = scope_bundle
            .map(|b| vec![(MODEL_BUNDLE_RESOURCE_TYPE.to_string(), b.to_string())])
            .unwrap_or_default();
        let (_id, uid) = crate::db::repository::create_api_key_with_scopes(
            db,
            &verifier,
            "sk-...cdef",
            "share-key",
            "general",
            None,
            60,
            &scopes,
            None,
            None,
        )
        .expect("create key");
        (raw.to_string(), uid)
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn bearer_resolves_valid_and_rejects_bad_key() {
        let db = fresh_db();
        let cipher = test_cipher();
        let (token, uid) = make_key(&db, &cipher, Some(BUNDLE_REF_ALL));
        match resolve_bearer_api_key(&db, &cipher, &token) {
            BearerAuthResult::Ok(row) => assert_eq!(row.uid, uid),
            _ => panic!("valid key must resolve"),
        }
        assert!(matches!(
            resolve_bearer_api_key(&db, &cipher, "sk-not-a-real-token"),
            BearerAuthResult::Invalid
        ));
    }

    #[test]
    fn api_key_scope_gate_allows_only_scoped_bundle() {
        let db = fresh_db();
        let cipher = test_cipher();
        let (_t, uid) = make_key(&db, &cipher, Some(BUNDLE_REF_ALL));
        // Scoped bundle → allowed; a different (structurally-valid) bundle → not.
        assert!(api_key_bundle_allowed(&db, BUNDLE_REF_ALL, &uid));
        assert!(!api_key_bundle_allowed(&db, "rfdetr-adr", &uid));
    }

    #[test]
    fn manifest_api_key_missing_scope_is_forbidden() {
        let db = fresh_db();
        let cipher = test_cipher();
        // Key scoped to `rfdetr-adr` only — asking for `vision-all` must 403,
        // and it must NOT collapse to NotFound (which would mean auth passed).
        let (_t, uid) = make_key(&db, &cipher, Some("rfdetr-adr"));
        let iss = issuer();
        let auth = BundleAuth::ApiKey { key_uid: &uid };
        let outcome = rt().block_on(handle_manifest(
            BUNDLE_REF_ALL,
            &auth,
            &iss,
            &db,
            RequestContext::default(),
        ));
        assert!(
            matches!(outcome, ManifestOutcome::Forbidden(_)),
            "missing scope must be Forbidden, got {outcome:?}"
        );
        assert_eq!(outcome.http_status(), 403);
    }

    #[test]
    fn manifest_api_key_with_scope_passes_auth() {
        let db = fresh_db();
        let cipher = test_cipher();
        let (_t, uid) = make_key(&db, &cipher, Some(BUNDLE_REF_ALL));
        let iss = issuer();
        let auth = BundleAuth::ApiKey { key_uid: &uid };
        let outcome = rt().block_on(handle_manifest(
            BUNDLE_REF_ALL,
            &auth,
            &iss,
            &db,
            RequestContext::default(),
        ));
        // Auth passed the scope gate: the only reasons left are Ok (files on
        // disk) or NotFound (empty vision dir) — never Forbidden/Denied.
        assert!(
            matches!(
                outcome,
                ManifestOutcome::Ok { .. } | ManifestOutcome::NotFound
            ),
            "scoped key must pass auth, got {outcome:?}"
        );
    }

    #[test]
    fn signed_url_manifest_still_verifies() {
        // The signed-URL path is unchanged by the Bearer addition.
        let db = fresh_db();
        let iss = issuer();
        let url = mint_model_bundle_url(&iss, BUNDLE_REF_ALL, 3600).expect("mint");
        let q = query_from_url(&url);
        let auth = BundleAuth::Signed(&q);
        let outcome = rt().block_on(handle_manifest(
            BUNDLE_REF_ALL,
            &auth,
            &iss,
            &db,
            RequestContext::default(),
        ));
        // A valid signed token clears auth; empty dir → NotFound, not Denied.
        assert!(
            !matches!(
                outcome,
                ManifestOutcome::Denied(_) | ManifestOutcome::Forbidden(_)
            ),
            "valid signed URL must clear auth, got {outcome:?}"
        );
    }
}
