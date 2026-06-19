// =============================================================================
// File: dispatch/legal_admin.rs
// Purpose: Admin-side binary RPCs for RODO/GDPR legal documents (F2 P8.c):
//          list, generate, revoke. Mirrors `dispatch::camera_admin` — single
//          dispatch slot under `MessageBody::LegalAdminBody`, per-org rate
//          limit on the expensive Generate path, permission gate via
//          `legal.read` / `legal.write` from `OrgContext`. Generate hands the
//          dashboard back a signed `/legal/<doc_id>?...` URL minted via the
//          `legal_url` HMAC issuer; the actual download goes through the REST
//          tier (`api/legal.rs`).
// =============================================================================

use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    LegalAdminPayload, LegalDocumentGenerateRequest, LegalDocumentGenerateResponse,
    LegalDocumentRevokeRequest, LegalDocumentRevokeResponse, LegalDocumentSummary,
    LegalDocumentsListRequest, LegalDocumentsListResponse, MessageBody, ProtocolError,
    ProtocolErrorCode,
};

use super::HandlerContext;
use crate::db::legal_documents::list_by_org;
use crate::services::legal::{
    generate_rodo, mint_legal_url, revoke_document_async, RodoGenerationError, RodoGenerationInput,
    RodoVariant,
};
use crate::services::rbac::OrgContext;

const PERM_READ: &str = "legal.read";
const PERM_WRITE: &str = "legal.write";

/// Per-org rate limit on Generate. Burst 3, sustain 3/min — a dashboard
/// operator clicking "Wygeneruj" four times in five seconds gets the fourth
/// request denied with `RateLimited`, the first three go through. The window
/// is generous enough for human operators, restrictive enough that a scripted
/// attacker cannot drain CPU through repeated PDF renders.
const GENERATE_BURST: u32 = 3;
const GENERATE_REFILL_PER_SEC: f64 = 3.0 / 60.0;

#[derive(Default)]
struct GenerateRateLimiter {
    buckets: dashmap::DashMap<String, Mutex<crate::util::token_bucket::TokenBucket>>,
}

impl GenerateRateLimiter {
    fn check(&self, org_id: &str) -> Result<(), f64> {
        let entry = self.buckets.entry(org_id.to_string()).or_insert_with(|| {
            Mutex::new(crate::util::token_bucket::TokenBucket::new(GENERATE_BURST))
        });
        let mut bucket = entry.lock();
        let now = Instant::now();
        bucket
            .refill_and_peek(GENERATE_BURST, GENERATE_REFILL_PER_SEC, now)
            .map(|()| bucket.commit_one())
    }
}

fn generate_rate_limiter() -> &'static Arc<GenerateRateLimiter> {
    static LIMITER: std::sync::OnceLock<Arc<GenerateRateLimiter>> = std::sync::OnceLock::new();
    LIMITER.get_or_init(|| Arc::new(GenerateRateLimiter::default()))
}

/// Test-only reset of the per-org bucket so unit tests can replay the burst
/// scenario without races against unrelated cases. Compiled unconditionally so
/// integration tests in sibling crates can reach it.
#[doc(hidden)]
pub fn reset_generate_rate_limiter_for_test() {
    generate_rate_limiter().buckets.clear();
}

fn require_org<'a>(ctx: &'a HandlerContext) -> Result<&'a OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

// =============================================================================
// Public dispatch entry — single `LegalAdminBody` slot
// =============================================================================

#[handler(variant = "LegalAdminBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn legal_admin_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::LegalAdminBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected LegalAdminBody")),
    };
    match payload {
        LegalAdminPayload::ListRequest(r) => {
            let resp = legal_documents_list_v1(ctx, r.clone()).await?;
            Ok(MessageBody::LegalAdminBody(
                LegalAdminPayload::ListResponse(resp),
            ))
        }
        LegalAdminPayload::GenerateRequest(r) => {
            let resp = legal_document_generate_v1(ctx, r.clone()).await?;
            Ok(MessageBody::LegalAdminBody(
                LegalAdminPayload::GenerateResponse(resp),
            ))
        }
        LegalAdminPayload::RevokeRequest(r) => {
            let resp = legal_document_revoke_v1(ctx, r.clone()).await?;
            Ok(MessageBody::LegalAdminBody(
                LegalAdminPayload::RevokeResponse(resp),
            ))
        }
        LegalAdminPayload::ListResponse(_)
        | LegalAdminPayload::GenerateResponse(_)
        | LegalAdminPayload::RevokeResponse(_) => Err(ProtocolError::bad_request(
            "response variant cannot be sent as a request",
        )),
    }
}

macro_rules! register_legal_admin_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_legal_admin_dispatch,
            }
        }
    };
}

register_legal_admin_variant!(
    "LegalDocumentsListRequest",
    "tentaflow_ws_handler_legal_list"
);
register_legal_admin_variant!(
    "LegalDocumentGenerateRequest",
    "tentaflow_ws_handler_legal_generate"
);
register_legal_admin_variant!(
    "LegalDocumentRevokeRequest",
    "tentaflow_ws_handler_legal_revoke"
);

// =============================================================================
// List
// =============================================================================

async fn legal_documents_list_v1(
    ctx: &HandlerContext,
    req: LegalDocumentsListRequest,
) -> Result<LegalDocumentsListResponse, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_READ) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "legal.read permission required",
        ));
    }
    let db = ctx.state.db.clone();
    let org_id = org.org_id.clone();
    let include_revoked = req.include_revoked;

    let rows = tokio::task::spawn_blocking(move || -> Result<_, ProtocolError> {
        let conn = db
            .read()
            .map_err(|_| ProtocolError::internal("db pool poisoned"))?;
        list_by_org(&conn, &org_id, include_revoked).map_err(|e| {
            tracing::warn!("legal_documents_list_v1 db error: {e}");
            ProtocolError::internal("db_error")
        })
    })
    .await
    .map_err(|join_err| ProtocolError::internal(format!("blocking task join: {join_err}")))??;

    let documents = rows
        .into_iter()
        .map(|d| LegalDocumentSummary {
            doc_id: d.id,
            org_id: d.org_id,
            variant: d.variant.as_str().to_string(),
            generated_at: d.generated_at,
            generated_by_user_id: d.generated_by_user_id,
            content_hash: d.content_hash,
            revoked_at_ms: d.revoked_at.unwrap_or(0),
        })
        .collect();
    Ok(LegalDocumentsListResponse { documents })
}

// =============================================================================
// Generate
// =============================================================================

async fn legal_document_generate_v1(
    ctx: &HandlerContext,
    req: LegalDocumentGenerateRequest,
) -> Result<LegalDocumentGenerateResponse, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_WRITE) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "legal.write permission required",
        ));
    }
    if generate_rate_limiter().check(&org.org_id).is_err() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::RateLimited,
            "legal.generate rate limit exceeded",
        ));
    }
    let variant = RodoVariant::from_str(&req.variant)
        .ok_or_else(|| ProtocolError::bad_request("variant must be short | standard | full"))?;

    let db = ctx.state.db.clone();
    let legal_root = crate::paths::legal_root_dir();
    let org_id = org.org_id.clone();
    let user_id = org.user_id.clone();
    let now_ms = chrono::Utc::now().timestamp_millis();

    let input = RodoGenerationInput {
        org_id: org_id.clone(),
        variant,
        generated_by_user_id: user_id.clone(),
    };

    let output = tokio::task::spawn_blocking(move || {
        let conn = db.write().map_err(|_| {
            RodoGenerationError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                "db pool poisoned",
            ))
        })?;
        generate_rodo(&conn, &legal_root, &input, now_ms)
    })
    .await
    .map_err(|join_err| ProtocolError::internal(format!("blocking task join: {join_err}")))?;

    let output = output.map_err(map_generate_error)?;

    // Mint the signed download URL (TTL 1 h). Persist `signed_url_ref` so
    // operators can still resolve the nonce later if audit asks.
    let issuer = crate::services::legal_url_issuer();
    let url = mint_legal_url(
        issuer,
        &output.doc_id,
        &org_id,
        crate::services::legal::DEFAULT_LEGAL_URL_TTL_SECS,
    )
    .map_err(|e| {
        tracing::warn!("legal mint_legal_url failed: {e}");
        ProtocolError::internal("signed_url_mint_failed")
    })?;

    // Best-effort: stash the nonce on the row for later forensics. A failure
    // here is logged and otherwise swallowed — the document is already on
    // disk and audited.
    let pool = ctx.state.db.clone();
    let doc_id_for_stash = output.doc_id.clone();
    let org_id_for_stash = org_id.clone();
    let nonce_for_stash = url.nonce.clone();
    let _ = tokio::task::spawn_blocking(move || {
        if let Ok(conn) = pool.write() {
            let _ = crate::db::legal_documents::set_signed_url_ref(
                &conn,
                &doc_id_for_stash,
                &org_id_for_stash,
                &nonce_for_stash,
            );
        }
    })
    .await;

    Ok(LegalDocumentGenerateResponse {
        doc_id: output.doc_id,
        content_hash: output.content_hash,
        signed_url: url.signed_url,
    })
}

fn map_generate_error(e: RodoGenerationError) -> ProtocolError {
    match e {
        RodoGenerationError::UserNotMember => {
            ProtocolError::new(ProtocolErrorCode::PolicyDenied, "user_not_member")
        }
        RodoGenerationError::TemplateRender(_) => ProtocolError::internal("template_render_failed"),
        RodoGenerationError::PdfGeneration(_) => ProtocolError::internal("pdf_generation_failed"),
        RodoGenerationError::Io(_) => ProtocolError::internal("io_error"),
        RodoGenerationError::Db(_) => ProtocolError::internal("db_error"),
        RodoGenerationError::PathTraversal => ProtocolError::bad_request("path_traversal_blocked"),
    }
}

// =============================================================================
// Revoke
// =============================================================================

async fn legal_document_revoke_v1(
    ctx: &HandlerContext,
    req: LegalDocumentRevokeRequest,
) -> Result<LegalDocumentRevokeResponse, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_WRITE) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "legal.write permission required",
        ));
    }
    if req.doc_id.is_empty() || req.doc_id.len() > 64 {
        return Err(ProtocolError::bad_request("doc_id_invalid"));
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    let result = revoke_document_async(
        ctx.state.db.clone(),
        org.org_id.clone(),
        req.doc_id.clone(),
        org.user_id.clone(),
        now_ms,
    )
    .await;
    match result {
        Ok(()) => Ok(LegalDocumentRevokeResponse {
            doc_id: req.doc_id,
            revoked_at_ms: now_ms,
        }),
        Err(crate::services::legal::RevokeError::NotFound) => {
            Err(ProtocolError::not_found("document_not_found"))
        }
        Err(crate::services::legal::RevokeError::UserNotMember) => Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "user_not_member",
        )),
        Err(crate::services::legal::RevokeError::AlreadyRevoked) => Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            "already_revoked",
        )),
        Err(crate::services::legal::RevokeError::Db(e)) => {
            tracing::warn!("legal revoke db error: {e}");
            Err(ProtocolError::internal("db_error"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_rate_limiter_per_org_burst() {
        // Burst 3 within tight window. Fourth call denied; reset isolates orgs.
        reset_generate_rate_limiter_for_test();
        let rl = generate_rate_limiter();
        let org_a = "11111111-1111-4111-8111-111111111111";
        let org_b = "22222222-2222-4222-8222-222222222222";
        for _ in 0..GENERATE_BURST {
            assert!(rl.check(org_a).is_ok());
        }
        assert!(rl.check(org_a).is_err(), "burst exhausted");
        // Distinct org keeps its own bucket.
        for _ in 0..GENERATE_BURST {
            assert!(rl.check(org_b).is_ok());
        }
    }
}
