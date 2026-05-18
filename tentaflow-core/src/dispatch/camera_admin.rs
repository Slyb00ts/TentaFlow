// =============================================================================
// File: dispatch/camera_admin.rs
// Purpose: Admin-side binary RPCs for camera discovery and ONVIF add (F2 P7.a).
//          Mirrors `camera_discover_v1` / `camera_add_v1` host-fn surface but
//          operates under a user session: permissions come from `OrgContext`
//          (camera.discover / camera.write), cameras are stamped with the
//          caller's org_id, and credentials never leave the binary protocol
//          plaintext — they are encrypted via `credentials_cipher` server-side
//          before persistence and never echoed back.
// =============================================================================

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    CameraAddOnvifRequest, CameraAddOnvifResponse, CameraAdminPayload, CameraDiscoverResponse,
    CameraFrameUrlRequest, CameraFrameUrlResponse, DiscoveredCameraInfo, MessageBody,
    ProtocolError, ProtocolErrorCode, SessionAuth,
};

use super::HandlerContext;
use crate::db::repository;
use crate::services::camera_ingest::credentials::credentials_cipher;
use crate::services::camera_ingest::onvif_discovery::{
    discover as ws_discover, DiscoveryOptions,
};
use crate::services::camera_ingest::onvif_media::{
    derive_rtsp_uri, OnvifCredentials, OnvifError,
};
use crate::services::rbac::OrgContext;

const PERM_DISCOVER: &str = "camera.discover";
const PERM_WRITE: &str = "camera.write";
const PERM_READ: &str = "camera.read";

// Frame URL dispatch contract — the dashboard `<tf-live-camera-tile>` may
// request TTLs as short as 5 s (preview refreshes every ttl/2 s). Values
// outside this band yield BadRequest before the issuer is touched.
const FRAME_URL_TTL_MIN_SECS: u32 = 5;
const FRAME_URL_TTL_MAX_SECS: u32 = 300;

// Per-user frame-url rate limit. Burst 30, sustain 30 / min — matches the
// browse-friendly tier (tile refresh is ttl/2 s; at the 5 s floor that
// produces 24 req/min for a single tile, leaving budget for ~1 extra tile
// before the bucket drains).
const FRAME_URL_BURST: u32 = 30;
const FRAME_URL_REFILL_PER_SEC: f64 = 30.0 / 60.0;

/// SOAP resolve budget — matches the host-fn one-click path.
const ONVIF_RESOLVE_TIMEOUT_MS: u32 = 10_000;
/// WS-Discovery collection window. Matches `DiscoveryOptions::default()`.
const DEFAULT_DISCOVER_TIMEOUT_MS: u64 = 3_000;

/// Tunable WS-Discovery collection window. Production keeps the default
/// 3 000 ms; integration tests shrink it to a few ms so they can drive the
/// rate-limit edge in well under a second.
static DISCOVER_TIMEOUT_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(DEFAULT_DISCOVER_TIMEOUT_MS);

/// Test-only seam: tightens the WS-Discovery wait so integration tests
/// (which live in a separate crate and therefore cannot use `#[cfg(test)]`
/// symbols) can drive the rate-limit window without burning 18 seconds.
/// Production callers must not touch this — the `_for_test` suffix and
/// `#[doc(hidden)]` keep it out of normal usage. A `feature = "test-seams"`
/// gate is intentionally NOT used because integration tests run with the
/// default feature set and a separate flag would require coordinated
/// dev-dependencies across consumers.
#[doc(hidden)]
pub fn set_discover_timeout_ms_for_test(ms: u64) {
    DISCOVER_TIMEOUT_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
}

/// Sentinel `owner_addon_id` used for admin-managed cameras inserted via the
/// dashboard wizard. The `cameras` table NOT NULL constraint on this column
/// dates back to the host-fn-only era; using a fixed marker lets reconciliation
/// + ownership queries distinguish admin-added rows from addon-managed ones
/// without a schema change.
const ADMIN_OWNER_ID: &str = "admin";

/// Default capture FPS when the request leaves the field unset. Mirrors the
/// host-fn default in `addon::host_functions::camera::default_target_fps`.
const DEFAULT_TARGET_FPS: u32 = 15;

/// Per-org WS-Discovery rate limit — broadcast WS-Discovery floods the LAN, so
/// the wizard must not be allowed to spam Probe envelopes from a tight UI loop.
/// Burst 6, sustain 6/min (one every 10 s). Matches the budget spec'd in the
/// F2 P7.a plan.
const DISCOVER_BURST: u32 = 6;
const DISCOVER_REFILL_PER_SEC: f64 = 6.0 / 60.0;

// =============================================================================
// Per-org rate limiter for camera.discover
// =============================================================================

#[derive(Default)]
struct DiscoverRateLimiter {
    buckets: dashmap::DashMap<String, Mutex<crate::util::token_bucket::TokenBucket>>,
}

impl DiscoverRateLimiter {
    fn check(&self, org_id: &str) -> Result<(), f64> {
        let entry = self
            .buckets
            .entry(org_id.to_string())
            .or_insert_with(|| Mutex::new(crate::util::token_bucket::TokenBucket::new(DISCOVER_BURST)));
        let mut bucket = entry.lock();
        let now = Instant::now();
        bucket
            .refill_and_peek(DISCOVER_BURST, DISCOVER_REFILL_PER_SEC, now)
            .map(|()| bucket.commit_one())
    }
}

fn discover_rate_limiter() -> &'static Arc<DiscoverRateLimiter> {
    static LIMITER: std::sync::OnceLock<Arc<DiscoverRateLimiter>> = std::sync::OnceLock::new();
    LIMITER.get_or_init(|| Arc::new(DiscoverRateLimiter::default()))
}

// =============================================================================
// Per-user rate limiter for camera.frame_url
// =============================================================================

#[derive(Default)]
struct FrameUrlRateLimiter {
    buckets: dashmap::DashMap<String, Mutex<crate::util::token_bucket::TokenBucket>>,
}

impl FrameUrlRateLimiter {
    fn check(&self, user_key: &str) -> Result<(), f64> {
        let entry = self
            .buckets
            .entry(user_key.to_string())
            .or_insert_with(|| {
                Mutex::new(crate::util::token_bucket::TokenBucket::new(FRAME_URL_BURST))
            });
        let mut bucket = entry.lock();
        let now = Instant::now();
        bucket
            .refill_and_peek(FRAME_URL_BURST, FRAME_URL_REFILL_PER_SEC, now)
            .map(|()| bucket.commit_one())
    }
}

fn frame_url_rate_limiter() -> &'static Arc<FrameUrlRateLimiter> {
    static LIMITER: std::sync::OnceLock<Arc<FrameUrlRateLimiter>> = std::sync::OnceLock::new();
    LIMITER.get_or_init(|| Arc::new(FrameUrlRateLimiter::default()))
}

/// Test-only reset of the per-user bucket so integration tests can replay
/// the burst scenario without cross-test contamination.
#[doc(hidden)]
pub fn reset_frame_url_rate_limiter_for_test() {
    frame_url_rate_limiter().buckets.clear();
}

/// Strict UUID v4 textual-form validator. Mirrors `validate_camera_id` in
/// `addon::ui_framework` (Chunk A). The contract is 36 chars, lowercase hex
/// plus dashes in the standard layout, version nibble `4` at index 14, and
/// the RFC 4122 variant nibble in the `8..=b` band at index 19. Kept local
/// rather than reaching across the `addon` module so the dispatch crate
/// doesn't drag in addon-only types.
fn validate_camera_id(id: &str) -> Result<(), &'static str> {
    if id.len() != 36 {
        return Err("camera_id_invalid_format");
    }
    let bytes = id.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        let dash_pos = matches!(i, 8 | 13 | 18 | 23);
        if dash_pos {
            if b != b'-' {
                return Err("camera_id_invalid_format");
            }
        } else if !(b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err("camera_id_invalid_format");
        }
    }
    if bytes[14] != b'4' {
        return Err("camera_id_invalid_format");
    }
    if !matches!(bytes[19], b'8' | b'9' | b'a' | b'b') {
        return Err("camera_id_invalid_format");
    }
    Ok(())
}

/// Test-only reset of the per-org bucket so tests can replay the burst scenario
/// without races against unrelated cases. Compiled into the library
/// unconditionally so integration tests (separate crates) can call it.
#[doc(hidden)]
pub fn reset_discover_rate_limiter_for_test() {
    discover_rate_limiter().buckets.clear();
}

// =============================================================================
// Helpers
// =============================================================================

fn require_org<'a>(ctx: &'a HandlerContext) -> Result<&'a OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

fn user_id_str(ctx: &HandlerContext) -> Option<&str> {
    match &ctx.session {
        SessionAuth::UserSession { .. } => ctx.org_context.as_ref().map(|o| o.user_id.as_str()),
        _ => None,
    }
}

fn audit_row(ctx: &HandlerContext, action: &str, resource: Option<&str>, details: &str) {
    let user_i64 = match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            if user_id[0] == 0xFF {
                let mut le = [0u8; 8];
                le.copy_from_slice(&user_id[8..]);
                Some(i64::from_le_bytes(le))
            } else {
                None
            }
        }
        _ => None,
    };
    let _ = repository::log_audit(
        &ctx.state.db,
        user_i64,
        None,
        action,
        resource,
        Some(details),
        None,
        Some(&ctx.state.local_node_id),
    );
}

fn validate_display_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("display_name_empty");
    }
    if name.len() > 200 {
        return Err("display_name_too_long");
    }
    Ok(())
}

fn validate_http_url(url: &str) -> Result<(), &'static str> {
    if url.is_empty() {
        return Err("url_empty");
    }
    if url.len() > 2048 {
        return Err("url_too_long");
    }
    // Parse via url::Url so userinfo / authority smuggling is rejected.
    // `http://camera.local@evil.host/onvif/...` would otherwise pass a
    // starts_with check yet target `evil.host`. We require http/https scheme,
    // a present host, and no embedded userinfo — credentials travel in the
    // SOAP envelope only, never inline in the device URL.
    let parsed = url::Url::parse(url).map_err(|_| "url_malformed")?;
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return Err("url_scheme_unsupported");
    }
    if parsed.host_str().map(str::is_empty).unwrap_or(true) {
        return Err("url_host_missing");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("url_userinfo_not_allowed");
    }
    Ok(())
}

fn validate_userpass(user: &str, pass: &str) -> Result<(), &'static str> {
    if user.is_empty() {
        return Err("username_empty");
    }
    if pass.is_empty() {
        return Err("password_empty");
    }
    // Same charset rules as `validate_userinfo_plaintext` in the host-fn —
    // the credentials end up overlaid into an `rtsp://user:pass@host/...`
    // URL when the pipeline starts; characters that require percent encoding
    // there are rejected here so a hostile dashboard call cannot smuggle a
    // user@evil.host segment into GStreamer.
    let safe = |c: char| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '-' | '.'
                    | '_'
                    | '~'
                    | '!'
                    | '$'
                    | '&'
                    | '\''
                    | '('
                    | ')'
                    | '*'
                    | '+'
                    | ','
                    | ';'
                    | '='
            )
    };
    if user.len() > 64 || !user.chars().all(safe) {
        return Err("username_invalid_chars");
    }
    if pass.len() > 128 || !pass.chars().all(safe) {
        return Err("password_invalid_chars");
    }
    Ok(())
}

fn validate_profile_token(token: &str) -> Result<(), &'static str> {
    if token.is_empty() || token.len() > 128 {
        return Err("profile_token_invalid");
    }
    if !token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("profile_token_invalid");
    }
    Ok(())
}

fn map_onvif_error(e: &OnvifError) -> ProtocolError {
    match e {
        OnvifError::AuthFailed => ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "onvif_auth_failed",
        ),
        OnvifError::NoProfiles => ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "onvif_no_profiles",
        ),
        OnvifError::ProfileNotFound(_) => ProtocolError::not_found("onvif_profile_not_found"),
        OnvifError::Timeout(_) => ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "onvif_timeout",
        ),
        OnvifError::Transport(_) => ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "onvif_transport_failure",
        ),
        OnvifError::SoapFault(_) | OnvifError::MalformedResponse(_) => {
            ProtocolError::bad_request("onvif_invalid_response")
        }
    }
}

fn map_onvif_error_tag(e: &OnvifError) -> &'static str {
    match e {
        OnvifError::AuthFailed => "onvif_auth_failed",
        OnvifError::NoProfiles => "onvif_no_profiles",
        OnvifError::ProfileNotFound(_) => "onvif_profile_not_found",
        OnvifError::Timeout(_) => "onvif_timeout",
        OnvifError::Transport(_) => "onvif_transport_failure",
        OnvifError::SoapFault(_) | OnvifError::MalformedResponse(_) => "onvif_invalid_response",
    }
}

// =============================================================================
// Public dispatch entry — single `CameraAdminBody` slot
// =============================================================================

#[handler(variant = "CameraAdminBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn camera_admin_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::CameraAdminBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected CameraAdminBody")),
    };
    match payload {
        CameraAdminPayload::DiscoverRequest(_) => {
            let resp = camera_discover(ctx).await?;
            Ok(MessageBody::CameraAdminBody(
                CameraAdminPayload::DiscoverResponse(resp),
            ))
        }
        CameraAdminPayload::AddOnvifRequest(r) => {
            let resp = camera_add_onvif(ctx, r.clone()).await?;
            Ok(MessageBody::CameraAdminBody(
                CameraAdminPayload::AddOnvifResponse(resp),
            ))
        }
        CameraAdminPayload::FrameUrlRequest(r) => {
            let resp = camera_frame_url(ctx, r.clone()).await?;
            Ok(MessageBody::CameraAdminBody(
                CameraAdminPayload::FrameUrlResponse(resp),
            ))
        }
        CameraAdminPayload::DiscoverResponse(_)
        | CameraAdminPayload::AddOnvifResponse(_)
        | CameraAdminPayload::FrameUrlResponse(_) => Err(ProtocolError::bad_request(
            "response variant cannot be sent as a request",
        )),
    }
}

macro_rules! register_camera_admin_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_camera_admin_dispatch,
            }
        }
    };
}

register_camera_admin_variant!(
    "CameraDiscoverRequest",
    "tentaflow_ws_handler_camera_discover"
);
register_camera_admin_variant!(
    "CameraAddOnvifRequest",
    "tentaflow_ws_handler_camera_add_onvif"
);
register_camera_admin_variant!(
    "CameraFrameUrlRequest",
    "tentaflow_ws_handler_camera_frame_url"
);

// =============================================================================
// Discover
// =============================================================================

async fn camera_discover(ctx: &HandlerContext) -> Result<CameraDiscoverResponse, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_DISCOVER) {
        audit_row(ctx, "camera.discover", None, "denied: missing_permission");
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "camera.discover permission required",
        ));
    }
    if discover_rate_limiter().check(&org.org_id).is_err() {
        audit_row(ctx, "camera.discover", None, "denied: rate_limited");
        return Err(ProtocolError::new(
            ProtocolErrorCode::RateLimited,
            "camera.discover rate limit exceeded",
        ));
    }

    let opts = DiscoveryOptions {
        timeout: Duration::from_millis(
            DISCOVER_TIMEOUT_MS.load(std::sync::atomic::Ordering::Relaxed),
        ),
        ..Default::default()
    };
    let discovered = match ws_discover(opts).await {
        Ok(list) => list
            .into_iter()
            .map(|c| DiscoveredCameraInfo {
                address: c.address,
                xaddrs: c.xaddrs,
                types: c.types,
                manufacturer: c.manufacturer,
                model: c.model,
            })
            .collect::<Vec<_>>(),
        Err(e) => {
            tracing::warn!("camera.discover ws-discovery failed: {e}");
            audit_row(ctx, "camera.discover", None, "error: ws_discovery_failed");
            Vec::new()
        }
    };
    audit_row(
        ctx,
        "camera.discover",
        None,
        &format!("ok: count={}", discovered.len()),
    );
    Ok(CameraDiscoverResponse { discovered })
}

// =============================================================================
// Add ONVIF
// =============================================================================

async fn camera_add_onvif(
    ctx: &HandlerContext,
    req: CameraAddOnvifRequest,
) -> Result<CameraAddOnvifResponse, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_WRITE) {
        audit_row(ctx, "camera.add_onvif", None, "denied: missing_permission");
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "camera.write permission required",
        ));
    }
    if let Err(reason) = validate_display_name(&req.display_name) {
        audit_row(ctx, "camera.add_onvif", None, &format!("denied: {reason}"));
        return Err(ProtocolError::bad_request(reason));
    }
    if let Err(reason) = validate_http_url(&req.device_service_url) {
        audit_row(ctx, "camera.add_onvif", None, &format!("denied: {reason}"));
        return Err(ProtocolError::bad_request(reason));
    }
    if let Err(reason) = validate_userpass(&req.username, &req.password) {
        audit_row(ctx, "camera.add_onvif", None, &format!("denied: {reason}"));
        return Err(ProtocolError::bad_request(reason));
    }
    if let Some(token) = req.profile_token.as_deref() {
        if let Err(reason) = validate_profile_token(token) {
            audit_row(ctx, "camera.add_onvif", None, &format!("denied: {reason}"));
            return Err(ProtocolError::bad_request(reason));
        }
    }
    let target_fps = req.target_fps.unwrap_or(DEFAULT_TARGET_FPS);
    if !(1..=60).contains(&target_fps) {
        audit_row(
            ctx,
            "camera.add_onvif",
            None,
            "denied: target_fps_out_of_range",
        );
        return Err(ProtocolError::bad_request("target_fps_out_of_range"));
    }

    // Encrypt credentials before doing anything that could panic / time out —
    // we never want a partially-resolved row to outlive the cipher.
    let plain = format!("{}:{}", req.username, req.password);
    let credentials_blob = credentials_cipher().encrypt(&plain).map_err(|_| {
        audit_row(
            ctx,
            "camera.add_onvif",
            None,
            "error: credentials_encrypt_failed",
        );
        ProtocolError::internal("credentials_encrypt_failed")
    })?;
    drop(plain);

    let creds = OnvifCredentials {
        username: req.username.clone(),
        password: req.password.clone(),
    };
    let resolved = derive_rtsp_uri(
        &req.device_service_url,
        &creds,
        req.profile_token.as_deref(),
        ONVIF_RESOLVE_TIMEOUT_MS,
    )
    .await
    .map_err(|e| {
        audit_row(
            ctx,
            "camera.add_onvif",
            None,
            &format!("error: {}", map_onvif_error_tag(&e)),
        );
        map_onvif_error(&e)
    })?;
    drop(creds);

    let camera_id = format!("cam_{}", uuid::Uuid::new_v4());

    // Insert the row first — supervisor wiring is deferred to the steady-state
    // reconciler (host-fn path starts the supervisor session inline; admin path
    // would have to plumb the live supervisor handle through `AppState`, which
    // P7.a does not require). Reconciliation picks the row up on next tick and
    // brings the session to Starting/Running.
    if let Err(e) = repository::insert_camera(
        &ctx.state.db,
        &camera_id,
        ADMIN_OWNER_ID,
        &req.display_name,
        "onvif",
        &resolved.rtsp_uri,
        target_fps as i64,
        None,
        None,
        "C",
        "default",
        Some(&credentials_blob),
        Some(&req.device_service_url),
        Some(&resolved.profile_token),
        Some(&org.org_id),
    ) {
        tracing::warn!("camera.add_onvif insert failed: {e}");
        audit_row(
            ctx,
            "camera.add_onvif",
            Some(&camera_id),
            "error: db_insert_failed",
        );
        return Err(ProtocolError::internal("db_insert_failed"));
    }

    audit_row(
        ctx,
        "camera.add_onvif",
        Some(&camera_id),
        &format!(
            "ok: user_id={} vendor=onvif",
            user_id_str(ctx).unwrap_or("?")
        ),
    );

    Ok(CameraAddOnvifResponse {
        camera_id,
        rtsp_url: resolved.rtsp_uri,
        profile_token: resolved.profile_token,
    })
}

// =============================================================================
// Frame URL (live tile)
// =============================================================================
//
// User-session counterpart to the addon-scoped `recording::frame_url_v1` host
// fn. Mints a same-origin signed `/frames/<ref>?token=...` URL for the latest
// frame stored for `camera_id` in the in-memory LRU. The browser tile
// (`<tf-live-camera-tile>`) calls this directly so panel rendering does not
// burn a round-trip through the addon WASM instance pool just to grab a URL.
//
// Security boundary:
//   - permission `camera.read` (gated against `OrgContext`),
//   - strict UUID v4 validation of `camera_id` (no echo of the raw value into
//     audit details — only `denied: <reason>` static tags),
//   - org_id isolation enforced at the DB query (`camera_exists_in_org`),
//   - per-user rate limit (burst 30, sustain 30/min),
//   - dispatch TTL band 5..=300 secs (BadRequest on out-of-range),
//   - HMAC mint goes through the global `frame_url_issuer()` — same key as
//     the addon-side path so URLs verify against the shared `/frames/` route.

async fn camera_frame_url(
    ctx: &HandlerContext,
    req: CameraFrameUrlRequest,
) -> Result<CameraFrameUrlResponse, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_READ) {
        audit_row(ctx, "camera.frame_url", None, "denied: missing_permission");
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "camera.read permission required",
        ));
    }
    if let Err(reason) = validate_camera_id(&req.camera_id) {
        audit_row(ctx, "camera.frame_url", None, &format!("denied: {reason}"));
        return Err(ProtocolError::bad_request(reason));
    }
    if req.ttl_secs < FRAME_URL_TTL_MIN_SECS || req.ttl_secs > FRAME_URL_TTL_MAX_SECS {
        audit_row(
            ctx,
            "camera.frame_url",
            None,
            "denied: ttl_secs_out_of_range",
        );
        return Err(ProtocolError::bad_request("ttl_secs_out_of_range"));
    }

    // Per-user bucket. Keyed by the org-scoped user id when available; fall
    // back to org_id alone (anonymous-but-org-bound paths must still throttle
    // even if user attribution is missing).
    let user_key = user_id_str(ctx)
        .map(|s| format!("{}:{}", org.org_id, s))
        .unwrap_or_else(|| format!("{}:_", org.org_id));
    if frame_url_rate_limiter().check(&user_key).is_err() {
        audit_row(ctx, "camera.frame_url", None, "denied: rate_limited");
        return Err(ProtocolError::new(
            ProtocolErrorCode::RateLimited,
            "camera.frame_url rate limit exceeded",
        ));
    }

    let exists = match crate::db::repository::camera_exists_in_org(
        &ctx.state.db,
        &req.camera_id,
        &org.org_id,
    ) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!("camera.frame_url db lookup failed: {e}");
            audit_row(ctx, "camera.frame_url", None, "error: db_query_failed");
            return Err(ProtocolError::internal("db_query_failed"));
        }
    };
    if !exists {
        // Static reason — never echo camera_id (cross-tenant probe defense).
        audit_row(ctx, "camera.frame_url", None, "denied: camera_not_found");
        return Err(ProtocolError::not_found("camera_not_found"));
    }

    let (frame_ref, _stored) = match crate::services::frame_storage()
        .latest_for_camera(&req.camera_id)
    {
        Some(p) => p,
        None => {
            audit_row(ctx, "camera.frame_url", None, "denied: no_frame_available");
            return Err(ProtocolError::not_found("no_frame_available"));
        }
    };

    let issued = match crate::services::frame_url_issuer()
        .issue(frame_ref.as_str().to_string(), req.ttl_secs as u64)
    {
        Ok(u) => u,
        Err(e) => {
            tracing::warn!("camera.frame_url issue failed: {e}");
            audit_row(ctx, "camera.frame_url", None, "error: issue_failed");
            return Err(ProtocolError::internal("issue_failed"));
        }
    };
    let signed_url = format!("/frames/{}?{}", frame_ref.as_str(), issued.query_string());
    audit_row(
        ctx,
        "camera.frame_url",
        Some(frame_ref.as_str()),
        &format!(
            "ok: user_id={} ttl={}",
            user_id_str(ctx).unwrap_or("?"),
            req.ttl_secs
        ),
    );

    Ok(CameraFrameUrlResponse {
        signed_url,
        expires_at_ms: issued.expiry_unix_ms as i64,
    })
}
