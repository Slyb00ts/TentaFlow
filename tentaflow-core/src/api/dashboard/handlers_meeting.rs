// =============================================================================
// File: api/dashboard/handlers_meeting.rs
// Opis: Handlery protokolu binarnego dla Meeting Bot. Wywoluja MeetingManager
//       (spawn kontenera, alokacja portow, zapis sesji w DB). Summary generator
//       wolan do router/chat dla modelu ustawionego w meeting_settings.
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MeetingActiveSessionResponse, MeetingPayload, MeetingSessionDescriptor,
    MeetingSessionDetailResponse, MeetingSessionLeaveResponse, MeetingSessionListResponse,
    MeetingSessionStartResponse, MeetingSessionSummaryEntry, MeetingSettingKv,
    MeetingSettingsGetResponse, MeetingSettingsUpdateResponse, MeetingSummaryGenerateResponse,
    MeetingTranscriptEntry, MeetingTranscriptsListResponse, MessageBody, ProtocolError,
    ProtocolErrorCode, SessionAuth,
};

use crate::db::repository;
use crate::dispatch::HandlerContext;
use crate::meeting::StartSessionRequest;

fn internal(e: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::internal(format!("meeting: {}", e))
}

fn bad_request(msg: &str) -> ProtocolError {
    ProtocolError::new(ProtocolErrorCode::InvalidFrame, msg.to_string())
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

fn meeting_payload<'a>(req: &'a MessageBody) -> Result<&'a MeetingPayload, ProtocolError> {
    match req {
        MessageBody::MeetingBody(p) => Ok(p),
        _ => Err(bad_request("expected MeetingBody")),
    }
}

/// Konwertuje descriptor z managera na protokolowy wariant (puste Option -> "").
fn desc_to_proto(d: crate::meeting::SessionDescriptor) -> MeetingSessionDescriptor {
    MeetingSessionDescriptor {
        session_id: d.session_id,
        meeting_key: d.meeting_key,
        meeting_url: d.meeting_url.unwrap_or_default(),
        title: d.title.unwrap_or_default(),
        status: d.status,
        started_at: d.started_at,
        last_activity_at: d.last_activity_at,
        ended_at: d.ended_at.unwrap_or_default(),
        platform: d.platform.unwrap_or_default(),
        entry_count: d.entry_count,
        quic_port: d.quic_port.map(|p| p as i32).unwrap_or(0),
        vnc_port: d.vnc_port.map(|p| p as i32).unwrap_or(0),
        novnc_port: d.novnc_port.map(|p| p as i32).unwrap_or(0),
        bot_endpoint_id: d.bot_endpoint_id.unwrap_or_default(),
        container_name: d.container_name.unwrap_or_default(),
        owner_user_id: d.owner_user_id.unwrap_or(0),
    }
}

fn empty_desc() -> MeetingSessionDescriptor {
    MeetingSessionDescriptor {
        session_id: 0,
        meeting_key: String::new(),
        meeting_url: String::new(),
        title: String::new(),
        status: String::new(),
        started_at: String::new(),
        last_activity_at: String::new(),
        ended_at: String::new(),
        platform: String::new(),
        entry_count: 0,
        quic_port: 0,
        vnc_port: 0,
        novnc_port: 0,
        bot_endpoint_id: String::new(),
        container_name: String::new(),
        owner_user_id: 0,
    }
}

fn row_to_entry(r: repository::transcripts::TranscriptRow) -> MeetingTranscriptEntry {
    MeetingTranscriptEntry {
        id: r.id,
        session_id: r.session_id,
        timestamp_ms: r.timestamp_ms,
        speaker: r.speaker,
        profile_id: r.profile_id.unwrap_or(0),
        confidence: r.confidence.unwrap_or(0.0),
        is_enrolled: r.is_enrolled,
        text: r.text,
        model: r.model,
    }
}

// =============================================================================
// 1. Session start
// =============================================================================

#[handler(variant = "MeetingSessionStartRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn meeting_session_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = meeting_payload(req)?;
    let MeetingPayload::ReqSessionStart(r) = payload else {
        return Err(bad_request("expected ReqSessionStart"));
    };
    if r.meeting_url.trim().is_empty() {
        return Err(bad_request("meeting_url wymagany"));
    }
    if r.meeting_url.len() > 2048 {
        return Err(bad_request("meeting_url za dlugi"));
    }
    let owner = current_user_id(ctx);
    let start = StartSessionRequest {
        meeting_url: r.meeting_url.clone(),
        title: if r.title.is_empty() {
            None
        } else {
            Some(r.title.clone())
        },
        platform: if r.platform.is_empty() {
            "teams".into()
        } else {
            r.platform.clone()
        },
        owner_user_id: owner,
        bot_name: if r.bot_name.is_empty() {
            "TentaFlow Bot".into()
        } else {
            r.bot_name.clone()
        },
        stt_alias: if r.stt_alias.is_empty() {
            None
        } else {
            Some(r.stt_alias.clone())
        },
        tts_alias: if r.tts_alias.is_empty() {
            None
        } else {
            Some(r.tts_alias.clone())
        },
        // Wire protocol ma jedno pole llm_alias — teams-bot (T1.5) rozdziela
        // LLM na summarization (końcowe podsumowanie) i flow (orchestrator).
        // Póki frontend/protocol nie dodadzą flow_alias osobno, pojedynczy wire
        // alias trafia do summarization; flow_alias używa domyślnego teams-flow.
        summarization_alias: if r.llm_alias.is_empty() {
            None
        } else {
            Some(r.llm_alias.clone())
        },
        flow_alias: None,
    };
    let desc = ctx
        .state
        .meeting_manager
        .start_session(start)
        .await
        .map_err(internal)?;
    Ok(MessageBody::MeetingBody(MeetingPayload::ResSessionStart(
        MeetingSessionStartResponse {
            session: desc_to_proto(desc),
        },
    )))
}

// =============================================================================
// 2. Session leave
// =============================================================================

#[handler(variant = "MeetingSessionLeaveRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn meeting_session_leave(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = meeting_payload(req)?;
    let MeetingPayload::ReqSessionLeave(r) = payload else {
        return Err(bad_request("expected ReqSessionLeave"));
    };
    ctx.state
        .meeting_manager
        .leave_session(r.session_id)
        .await
        .map_err(internal)?;
    Ok(MessageBody::MeetingBody(MeetingPayload::ResSessionLeave(
        MeetingSessionLeaveResponse { ok: true },
    )))
}

// =============================================================================
// 3. Session list
// =============================================================================

#[handler(variant = "MeetingSessionListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn meeting_session_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = meeting_payload(req)?;
    let MeetingPayload::ReqSessionList(r) = payload else {
        return Err(bad_request("expected ReqSessionList"));
    };
    // BOLA: only_mine=false widzi wszystkie sesje — wymagamy admina. User bez
    // admina dostaje tylko swoje sesje niezależnie od flagi.
    let uid = current_user_id(ctx);
    let owner = if !r.only_mine && is_admin(ctx) {
        None
    } else {
        uid
    };
    let sessions = ctx
        .state
        .meeting_manager
        .session_list(owner)
        .map_err(internal)?
        .into_iter()
        .map(desc_to_proto)
        .collect();
    Ok(MessageBody::MeetingBody(MeetingPayload::ResSessionList(
        MeetingSessionListResponse { sessions },
    )))
}

fn is_admin(ctx: &HandlerContext) -> bool {
    matches!(
        &ctx.session,
        SessionAuth::UserSession { role: Some(r), .. } if r == "admin"
    )
}

// =============================================================================
// 4. Session detail
// =============================================================================

#[handler(variant = "MeetingSessionDetailRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn meeting_session_detail(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = meeting_payload(req)?;
    let MeetingPayload::ReqSessionDetail(r) = payload else {
        return Err(bad_request("expected ReqSessionDetail"));
    };
    let desc = ctx
        .state
        .meeting_manager
        .session_detail(r.session_id)
        .map_err(internal)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "session not found"))?;
    // BOLA: sesja widziana tylko przez ownera lub admina. Jesli owner_user_id
    // nie ustawione (legacy lub zewnetrzny ingest) — admin only.
    if !is_admin(ctx) {
        let me = current_user_id(ctx);
        if desc.owner_user_id.is_none() || me != desc.owner_user_id {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                "nie masz dostepu do tej sesji",
            ));
        }
    }
    let transcripts = if r.include_transcripts {
        repository::transcripts::list_transcripts(&ctx.state.db, r.session_id)
            .map_err(internal)?
            .into_iter()
            .map(row_to_entry)
            .collect()
    } else {
        Vec::new()
    };
    let summary = ctx
        .state
        .meeting_manager
        .summary(r.session_id)
        .map_err(internal)?;
    let resp = MeetingSessionDetailResponse {
        session: desc_to_proto(desc),
        transcripts,
        summary_tldr: summary.as_ref().map(|s| s.tldr.clone()).unwrap_or_default(),
        summary_decisions: summary
            .as_ref()
            .map(|s| s.decisions.clone())
            .unwrap_or_default(),
        summary_action_items_json: summary
            .as_ref()
            .map(|s| s.action_items_json.clone())
            .unwrap_or_else(|| "[]".into()),
        summary_open_questions: summary
            .as_ref()
            .map(|s| s.open_questions.clone())
            .unwrap_or_default(),
        summary_model: summary
            .as_ref()
            .map(|s| s.model.clone())
            .unwrap_or_default(),
        summary_generated_at: summary
            .as_ref()
            .map(|s| s.generated_at.clone())
            .unwrap_or_default(),
    };
    Ok(MessageBody::MeetingBody(MeetingPayload::ResSessionDetail(
        resp,
    )))
}

// =============================================================================
// 5. Transcripts list (polled during live session)
// =============================================================================

#[handler(variant = "MeetingTranscriptsListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn meeting_transcripts_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = meeting_payload(req)?;
    let MeetingPayload::ReqTranscriptsList(r) = payload else {
        return Err(bad_request("expected ReqTranscriptsList"));
    };
    let all =
        repository::transcripts::list_transcripts(&ctx.state.db, r.session_id).map_err(internal)?;
    let entries: Vec<MeetingTranscriptEntry> = all
        .into_iter()
        .filter(|t| r.since_ms == 0 || t.timestamp_ms > r.since_ms)
        .map(row_to_entry)
        .collect();
    Ok(MessageBody::MeetingBody(
        MeetingPayload::ResTranscriptsList(MeetingTranscriptsListResponse { entries }),
    ))
}

// =============================================================================
// 6. Summary generate (LLM)
// =============================================================================

#[handler(variant = "MeetingSummaryGenerateRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn meeting_summary_generate(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = meeting_payload(req)?;
    let MeetingPayload::ReqSummaryGenerate(r) = payload else {
        return Err(bad_request("expected ReqSummaryGenerate"));
    };
    // Cache: jesli mamy summary i nie force — zwroc istniejace.
    if !r.force_refresh {
        if let Some(s) = ctx
            .state
            .meeting_manager
            .summary(r.session_id)
            .map_err(internal)?
        {
            return Ok(to_summary_response(s));
        }
    }
    // Weryfikuj ze sesja ma transkrypty do podsumowania.
    let rows =
        repository::transcripts::list_transcripts(&ctx.state.db, r.session_id).map_err(internal)?;
    if rows.is_empty() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotFound,
            "brak transkryptow — nie ma co podsumowywac",
        ));
    }
    // Resolve alias `teams-summary` — pusty target = admin nie skonfigurowal modelu.
    // Handler zwraca jawny error, frontend pokazuje info "summary wylaczone".
    let alias =
        repository::resolve_model_alias(&ctx.state.db, "teams-summary").map_err(internal)?;
    let target = alias
        .as_ref()
        .map(|a| a.target_model.trim().to_string())
        .unwrap_or_default();
    if target.is_empty() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotImplemented,
            "Summary wylaczone — wpisz target dla aliasu 'teams-summary' w Models > Aliases",
        ));
    }
    // TODO(meeting-bot LLM): integracja z router.chat_completion. Na razie
    // zwracamy jawny NotImplemented z nazwa modelu — admin wie ze alias jest OK
    // ale kod LLM calla jeszcze nie wpiety. Zadne fake TLDR sie nie zapisuje.
    Err(ProtocolError::new(
        ProtocolErrorCode::NotImplemented,
        format!(
            "Alias teams-summary ustawiony na '{}' — integracja LLM w toku, na razie summary niedostepne",
            target
        ),
    ))
}

fn to_summary_response(s: crate::meeting::SessionSummary) -> MessageBody {
    MessageBody::MeetingBody(MeetingPayload::ResSummaryGenerate(
        MeetingSummaryGenerateResponse {
            summary: MeetingSessionSummaryEntry {
                tldr: s.tldr,
                decisions: s.decisions,
                action_items_json: s.action_items_json,
                open_questions: s.open_questions,
                model: s.model,
                generated_at: s.generated_at,
            },
        },
    ))
}

// =============================================================================
// 7. Active session — UI refresh helper
// =============================================================================

#[handler(variant = "MeetingActiveSessionRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn meeting_active_session(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let uid = current_user_id(ctx).ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "session missing user_id")
    })?;
    let active = ctx
        .state
        .meeting_manager
        .active_for_user(uid)
        .map_err(internal)?;
    let resp = match active {
        Some(d) => MeetingActiveSessionResponse {
            has_active: true,
            session: desc_to_proto(d),
        },
        None => MeetingActiveSessionResponse {
            has_active: false,
            session: empty_desc(),
        },
    };
    Ok(MessageBody::MeetingBody(MeetingPayload::ResActiveSession(
        resp,
    )))
}

// =============================================================================
// 8. Settings get
// =============================================================================

#[handler(variant = "MeetingSettingsGetRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn meeting_settings_get(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let uid = current_user_id(ctx).ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "session missing user_id")
    })?;
    let rows = repository::transcripts::list_user_settings(&ctx.state.db, uid).map_err(internal)?;
    let settings = rows
        .into_iter()
        .map(|(k, v)| MeetingSettingKv { key: k, value: v })
        .collect();
    Ok(MessageBody::MeetingBody(MeetingPayload::ResSettingsGet(
        MeetingSettingsGetResponse { settings },
    )))
}

// =============================================================================
// 9. Settings update
// =============================================================================

#[handler(variant = "MeetingSettingsUpdateRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn meeting_settings_update(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = meeting_payload(req)?;
    let MeetingPayload::ReqSettingsUpdate(r) = payload else {
        return Err(bad_request("expected ReqSettingsUpdate"));
    };
    let uid = current_user_id(ctx).ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "session missing user_id")
    })?;
    for kv in &r.settings {
        if kv.key.is_empty() || kv.key.len() > 128 {
            return Err(bad_request("key pusty lub >128 znakow"));
        }
        if kv.value.len() > 1024 {
            return Err(bad_request("value >1024 znakow"));
        }
        repository::transcripts::set_user_setting(&ctx.state.db, uid, &kv.key, &kv.value)
            .map_err(internal)?;
    }
    Ok(MessageBody::MeetingBody(MeetingPayload::ResSettingsUpdate(
        MeetingSettingsUpdateResponse { ok: true },
    )))
}
