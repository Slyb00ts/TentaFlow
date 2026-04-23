// =============================================================================
// File: api/dashboard/handlers_notes.rs — Per-user Notes CRUD handlers.
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, NoteCreateResponse, NoteDeleteResponse, NoteDetailResponse, NoteEntry,
    NoteSetPinnedResponse, NoteUpdateResponse, NotesListResponse, NotesRequest, NotesResponse,
    ProtocolError, ProtocolErrorCode, SessionAuth,
};

use crate::db::repository;
use crate::dispatch::HandlerContext;

const PREVIEW_MAX_CHARS: usize = 200;

fn db_err(e: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::internal(format!("database error: {}", e))
}

fn current_user_id(ctx: &HandlerContext) -> Option<i64> {
    match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            if user_id[0] != 0xFF {
                return None;
            }
            let mut le = [0u8; 8];
            le.copy_from_slice(&user_id[8..]);
            Some(i64::from_le_bytes(le))
        }
        _ => None,
    }
}

/// Audit stub for Notes writes. Logs action + note_id only — never the title
/// or body content, to avoid leaking private text into the audit stream.
fn audit(ctx: &HandlerContext, action: &str, note_id: Option<i64>) {
    let user_id = current_user_id(ctx);
    let details = match note_id {
        Some(id) => format!(r#"{{"note_id":{},"action":"{}"}}"#, id, action),
        None => format!(r#"{{"action":"{}"}}"#, action),
    };
    let resource_id = note_id.map(|id| id.to_string());
    let node_id = ctx.state.local_node_id.as_ref();
    if let Err(e) = repository::log_audit_full(
        &ctx.state.db,
        user_id,
        None,
        action,
        Some("note"),
        resource_id.as_deref(),
        Some(&details),
        "info",
        None,
        Some(node_id),
    ) {
        tracing::warn!("notes audit log failed ({}): {}", action, e);
    }
}

fn require_user(ctx: &HandlerContext) -> Result<i64, ProtocolError> {
    current_user_id(ctx).ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::AuthRequired,
            "missing user_id in session",
        )
    })
}

fn body_preview(body: &str) -> String {
    if body.chars().count() <= PREVIEW_MAX_CHARS {
        return body.to_string();
    }
    body.chars().take(PREVIEW_MAX_CHARS).collect()
}

fn notes_req<'a>(req: &'a MessageBody) -> Result<&'a NotesRequest, ProtocolError> {
    match req {
        MessageBody::NotesRequestBody(r) => Ok(r),
        _ => Err(ProtocolError::new(
            ProtocolErrorCode::InvalidFrame,
            "expected NotesRequest",
        )),
    }
}

fn not_found_err(e: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::new(
        ProtocolErrorCode::NotFound,
        format!("note not found or not owned: {}", e),
    )
}

#[handler(variant = "NotesRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn notes_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let uid = require_user(ctx)?;
    let inner = notes_req(req)?;
    let out = match inner {
        NotesRequest::List(_) => {
            let rows = repository::list_notes_for_user(&ctx.state.db, uid).map_err(db_err)?;
            let notes = rows
                .into_iter()
                .map(|n| NoteEntry {
                    id: n.id,
                    title: n.title,
                    body_preview: body_preview(&n.body),
                    pinned: n.pinned,
                    created_at_epoch: n.created_at_epoch,
                    updated_at_epoch: n.updated_at_epoch,
                })
                .collect();
            NotesResponse::List(NotesListResponse { notes })
        }
        NotesRequest::Detail(r) => {
            let note = repository::get_note(&ctx.state.db, r.note_id, uid)
                .map_err(db_err)?
                .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "note not found"))?;
            NotesResponse::Detail(NoteDetailResponse {
                id: note.id,
                title: note.title,
                body: note.body,
                pinned: note.pinned,
                created_at_epoch: note.created_at_epoch,
                updated_at_epoch: note.updated_at_epoch,
            })
        }
        NotesRequest::Create(r) => {
            let id =
                repository::create_note(&ctx.state.db, uid, &r.title, &r.body).map_err(db_err)?;
            audit(ctx, "note_create", Some(id));
            NotesResponse::Create(NoteCreateResponse { id })
        }
        NotesRequest::Update(r) => {
            repository::update_note(&ctx.state.db, r.note_id, uid, &r.title, &r.body)
                .map_err(not_found_err)?;
            let updated = repository::get_note(&ctx.state.db, r.note_id, uid)
                .map_err(db_err)?
                .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "note vanished"))?;
            audit(ctx, "note_update", Some(r.note_id));
            NotesResponse::Update(NoteUpdateResponse {
                ok: true,
                updated_at_epoch: updated.updated_at_epoch,
            })
        }
        NotesRequest::SetPinned(r) => {
            repository::set_note_pinned(&ctx.state.db, r.note_id, uid, r.pinned)
                .map_err(not_found_err)?;
            audit(
                ctx,
                if r.pinned { "note_pin" } else { "note_unpin" },
                Some(r.note_id),
            );
            NotesResponse::SetPinned(NoteSetPinnedResponse { ok: true })
        }
        NotesRequest::Delete(r) => {
            repository::delete_note(&ctx.state.db, r.note_id, uid).map_err(not_found_err)?;
            audit(ctx, "note_delete", Some(r.note_id));
            NotesResponse::Delete(NoteDeleteResponse { ok: true })
        }
    };
    Ok(MessageBody::NotesResponseBody(out))
}
