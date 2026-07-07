// =============================================================================
// File: dispatch/addon_document_upload.rs — generic FileInput → addon document store
//
// Most uploadu plików z paneli UI addonów. Renderer FileInput w panelu addona
// emituje TYLKO metadane wybranych plików; bajty wgrywa HOST (frontend) dzieląc
// plik na fragmenty `seq` (0..total_chunks) o wspólnym `upload_id`. Ten handler
// akumuluje fragmenty w document store instancji `addon_id` i po ostatnim
// fragmencie zwraca `doc_ref` (id bloba) — addon czyta bajty przez
// `document_get` / `ingest_document`. Zapis idzie przez TĘ SAMĄ warstwę co
// host-fn `document_put_v1` (`accept_upload_chunk_host`), więc upload z panelu i
// odczyt addona współdzielą jeden content-addressed store i jedną serializację.
//
// Izolacja multi-tenant (TWARDA): `org_id` bierzemy z uwierzytelnionej sesji
// (NIE z requestu), `addon_id` walidujemy podwójnie — (1) połączenie MUSI mieć
// otwarty panel tego addona (ten sam model własności co ścieżka `Action`), oraz
// (2) org instancji addona (`instance_org_id`) MUSI równać się org sesji.
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    AddonDocumentPayload, AddonDocumentUploadChunkResponse, MessageBody, ProtocolError,
    ProtocolErrorCode, SessionAuth,
};

use super::ui_channel::run_blocking;
use super::HandlerContext;
use crate::addon::host_functions::document::{
    accept_upload_chunk_host, document_storage_limit_mb, HostUploadOutcome,
};
use crate::addon::errors::AbiError;

/// Górny limit liczby fragmentów (zgodny z document store `MAX_TOTAL_CHUNKS`).
const MAX_TOTAL_CHUNKS: u32 = 100_000;
/// Maksymalna długość `upload_id` (sanity — trafia do nazwy partiala).
const MAX_UPLOAD_ID_LEN: usize = 128;
/// Maksymalna długość `mime` (sanity).
const MAX_MIME_LEN: usize = 255;

fn org_from_session(ctx: &HandlerContext) -> Result<&crate::services::rbac::OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

/// Mapuje błąd warstwy document store na `ProtocolError`. QuotaExceeded →
/// TooManyRequests (klient powinien przerwać), reszta → BadRequest/Internal.
fn map_store_err(reason: &'static str, err: AbiError) -> ProtocolError {
    match err {
        AbiError::QuotaExceeded => {
            ProtocolError::new(ProtocolErrorCode::RateLimited, format!("upload rejected: {reason}"))
        }
        AbiError::PayloadTooLarge => ProtocolError::bad_request(format!("chunk too large: {reason}")),
        AbiError::Operation => ProtocolError::bad_request(format!("upload error: {reason}")),
        _ => ProtocolError::internal(format!("upload failed: {reason}")),
    }
}

#[handler(variant = "AddonDocumentUploadChunkRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn addon_document_upload_chunk(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::AddonDocumentBody(AddonDocumentPayload::UploadChunkRequest(p)) => p,
        _ => return Err(ProtocolError::bad_request("expected AddonDocumentUploadChunkRequest")),
    };

    // org z UWIERZYTELNIONEJ sesji — NIGDY z requestu.
    let org = org_from_session(ctx)?;
    let org_id = org.org_id.clone();

    // Sanity pól requestu (tanie odrzucenie przed dotknięciem store).
    if payload.addon_id.trim().is_empty() {
        return Err(ProtocolError::bad_request("addon_id required"));
    }
    if payload.upload_id.trim().is_empty() || payload.upload_id.len() > MAX_UPLOAD_ID_LEN {
        return Err(ProtocolError::bad_request("invalid upload_id"));
    }
    if payload.total_chunks == 0 || payload.total_chunks > MAX_TOTAL_CHUNKS {
        return Err(ProtocolError::bad_request("invalid total_chunks"));
    }
    if payload.seq >= payload.total_chunks {
        return Err(ProtocolError::bad_request("seq out of range"));
    }
    if payload.mime.len() > MAX_MIME_LEN {
        return Err(ProtocolError::bad_request("mime too long"));
    }

    // Izolacja (1): połączenie MUSI mieć otwarty panel tego addona na tym sockecie
    // (ten sam model własności co ścieżka Action). Bez otwartego panelu upload nie
    // ma kontekstu i jest odrzucany.
    {
        let session_lock = ctx.state.ui_sessions.get_or_create(ctx.connection_id);
        let session = session_lock.lock();
        if !session.has_open_panel_for_addon(&payload.addon_id) {
            return Err(ProtocolError::bad_request(format!(
                "no open panel for addon '{}'",
                payload.addon_id
            )));
        }
    }

    // Izolacja (2): org running-instancji addona MUSI równać się org sesji —
    // addon manager trzyma org wpisany przy starcie instancji. Brak instancji →
    // panel nie mógł zostać uruchomiony, więc to stan niespójny → odrzuć.
    let addon_mgr = ctx
        .state
        .addon_manager
        .as_ref()
        .ok_or_else(|| ProtocolError::internal("addon manager not configured"))?;
    match addon_mgr.instance_org_id(&payload.addon_id) {
        Some(inst_org) if inst_org == org_id => {}
        Some(_) => {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                "addon instance belongs to a different org",
            ));
        }
        None => {
            return Err(ProtocolError::bad_request(format!(
                "addon '{}' has no running instance",
                payload.addon_id
            )));
        }
    }

    // Sanity sesji: tylko user session (nie API key / mesh) — panel UI istnieje
    // wyłącznie dla użytkownika.
    let user_id = match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => uuid::Uuid::from_bytes(*user_id).to_string(),
        _ => {
            return Err(ProtocolError::new(
                ProtocolErrorCode::AuthRequired,
                "user session required",
            ))
        }
    };

    // Przechwyt mikrofonu (komponent AudioCapture) wymaga uprawnienia
    // `audio.capture`. Bramkujemy na ZAUFANYM markerze źródła `source ==
    // "audio_capture"` ustawianym WYŁĄCZNIE przez renderer AudioCapture, a NIE na
    // stringu MIME dostarczanym przez klienta. Gdyby gate ufał MIME, ten sam
    // kanał document-upload przenoszący nagranie mikrofonu z `mime =
    // "application/octet-stream"` ominąłby bramkę. Marker jest niepodrabialny
    // przez guest addon: addon nie kontroluje wywołania kanału upload (robi to
    // zaufany renderer per typ komponentu). Zwykły upload plików (FileInput) ma
    // `source` puste i NIE wymaga tego uprawnienia — wybór pliku audio to nie
    // przechwycenie mikrofonu. Gate PRZED dotknięciem store (fail-closed).
    if payload.source == tentaflow_protocol::UPLOAD_SOURCE_AUDIO_CAPTURE
        && !addon_mgr
            .permission_checker()
            .check(&payload.addon_id, &user_id, "audio.capture", None)
            .is_granted()
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "addon lacks 'audio.capture' permission for microphone capture",
        ));
    }

    // Limit `document_storage_mb` z globalnej DB (jeden punkt prawdy z host-fn).
    let limit_mb = document_storage_limit_mb(&ctx.state.db, &payload.addon_id);

    // Akceptacja fragmentu + ewentualna finalizacja — disk IO + (na ostatnim
    // fragmencie) strumieniowe hashowanie, więc poza async workerem.
    let addon_id = payload.addon_id.clone();
    let upload_id = payload.upload_id.clone();
    let mime = payload.mime.clone();
    let seq = payload.seq;
    let total_chunks = payload.total_chunks;
    let bytes = payload.bytes.clone();
    let outcome = run_blocking(move || {
        accept_upload_chunk_host(
            &org_id,
            &addon_id,
            &upload_id,
            &mime,
            seq,
            total_chunks,
            &bytes,
            limit_mb,
        )
    })
    .map_err(|(reason, err)| map_store_err(reason, err))?;

    let received_bytes = match &outcome {
        HostUploadOutcome::Buffered { .. } => 0,
        HostUploadOutcome::Finalized { size_bytes, .. } => *size_bytes,
    };
    let doc_ref = match outcome {
        HostUploadOutcome::Buffered { .. } => None,
        HostUploadOutcome::Finalized { doc_ref, .. } => Some(doc_ref),
    };

    Ok(MessageBody::AddonDocumentBody(
        AddonDocumentPayload::UploadChunkResponse(AddonDocumentUploadChunkResponse {
            upload_id: payload.upload_id.clone(),
            received_chunks: payload.seq + 1,
            received_bytes,
            doc_ref,
        }),
    ))
}
