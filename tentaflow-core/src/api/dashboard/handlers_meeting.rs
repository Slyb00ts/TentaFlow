// =============================================================================
// File: api/dashboard/handlers_meeting.rs
// Opis: Handlery protokolu binarnego dla Meeting Bot. Wywoluja MeetingManager
//       (spawn kontenera, alokacja portow, zapis sesji w DB). Summary generator
//       wolan do router/chat dla modelu ustawionego w meeting_settings.
// =============================================================================

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MeetingActionItemItem, MeetingActionItemStatusUpdateResponse, MeetingActionItemsListResponse,
    MeetingActiveSessionResponse, MeetingPayload, MeetingSessionDescriptor,
    MeetingSessionDetailResponse, MeetingSessionLeaveResponse, MeetingSessionListResponse,
    MeetingSessionStartResponse, MeetingSettingKv,
    MeetingSettingsGetResponse, MeetingSettingsUpdateResponse,
    MeetingSummariesListResponse, MeetingSummaryItem,
    MeetingTranscriptEntry, MeetingTranscriptExportResponse, MeetingTranscriptsListResponse,
    MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth,
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
        lifecycle_stage: d.lifecycle_stage.unwrap_or_default(),
        lifecycle_details: d.lifecycle_details.unwrap_or_default(),
        backend_stt_model: d.backend_stt_model.unwrap_or_default(),
        backend_tts_model: d.backend_tts_model.unwrap_or_default(),
        backend_summarization_model: d.backend_summarization_model.unwrap_or_default(),
        backend_diarization_model: d.backend_diarization_model.unwrap_or_default(),
        backend_streaming_latency_ms: d.backend_streaming_latency_ms.unwrap_or(-1),
        backend_enrolled_speakers: d.backend_enrolled_speakers.unwrap_or(-1),
        backend_total_participants: d.backend_total_participants.unwrap_or(-1),
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
        lifecycle_stage: String::new(),
        lifecycle_details: String::new(),
        backend_stt_model: String::new(),
        backend_tts_model: String::new(),
        backend_summarization_model: String::new(),
        backend_diarization_model: String::new(),
        backend_streaming_latency_ms: -1,
        backend_enrolled_speakers: -1,
        backend_total_participants: -1,
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
        // Wire protocol ma jedno pole llm_alias — teams-bot rozdziela LLM na:
        //   * summarization (okresowe podsumowanie, default teams-summarization)
        //   * llm (real-time odpowiedzi bota, default teams-llm)
        //   * flow (orchestrator, default teams-flow)
        // Aktualnie wire `llm_alias` mapuje sie i na summarization i na llm
        // jednoczesnie — caller posylajacy `llm_alias` chce miec ten sam model
        // na obu rolach. Operator moze pozniej rozdzielic w dashboardzie.
        summarization_alias: if r.llm_alias.is_empty() {
            None
        } else {
            Some(r.llm_alias.clone())
        },
        flow_alias: None,
        llm_alias: if r.llm_alias.is_empty() {
            None
        } else {
            Some(r.llm_alias.clone())
        },
        // Bot odpowiada w real-time tylko gdy caller jawnie poda llm_alias.
        // Dashboard moze dodac osobny przycisk respond_enabled.
        respond_enabled: if r.llm_alias.is_empty() { Some(false) } else { Some(true) },
        // Default: pasywny tryb wake_word_intent (bot odpowiada tylko gdy
        // ktos powie "jarvis"/"asystencie" + LLM uzna to za realne pytanie).
        // Dashboard moze nadpisac jezeli protocol zostanie rozszerzony.
        response_mode: None,
        wake_words: None,
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
    let resp = MeetingSessionDetailResponse {
        session: desc_to_proto(desc),
        transcripts,
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
// 6. Active session — UI refresh helper
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

// =============================================================================
// 10. Summaries list (po-sesyjne, historyczne)
// =============================================================================

/// Weryfikuje ownership sesji i zwraca session_id. Admin widzi wszystko.
/// Sesje bez ownera (legacy) — tylko admin.
fn resolve_owned_session_id(
    ctx: &HandlerContext,
    meeting_key: &str,
) -> Result<i64, ProtocolError> {
    let session_id = repository::transcripts::session_id_by_meeting_key(&ctx.state.db, meeting_key)
        .map_err(internal)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "meeting session not found"))?;
    if is_admin(ctx) {
        return Ok(session_id);
    }
    let me = current_user_id(ctx).ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "session missing user_id")
    })?;
    let owner = repository::transcripts::owner_of_meeting_key(&ctx.state.db, meeting_key)
        .map_err(internal)?
        .flatten();
    if owner != Some(me) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "nie masz dostepu do tej sesji",
        ));
    }
    Ok(session_id)
}

#[handler(variant = "MeetingSummariesListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn meeting_summaries_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = meeting_payload(req)?;
    let MeetingPayload::ReqSummariesList(r) = payload else {
        return Err(bad_request("expected ReqSummariesList"));
    };
    let session_id = resolve_owned_session_id(ctx, &r.meeting_key)?;
    let limit = r.limit.unwrap_or(20).max(1);
    let rows = repository::transcripts::list_summaries_for_meeting(&ctx.state.db, session_id, limit)
        .map_err(internal)?;
    let items = rows
        .into_iter()
        .map(|s| MeetingSummaryItem {
            id: s.id,
            created_at: s.created_at,
            decisions_text: s.decisions_text,
            summary_text: s.summary_text,
            model: s.model,
        })
        .collect();
    Ok(MessageBody::MeetingBody(MeetingPayload::ResSummariesList(
        MeetingSummariesListResponse { items },
    )))
}

// =============================================================================
// 11. Action items list
// =============================================================================

#[handler(variant = "MeetingActionItemsListRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn meeting_action_items_list(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = meeting_payload(req)?;
    let MeetingPayload::ReqActionItemsList(r) = payload else {
        return Err(bad_request("expected ReqActionItemsList"));
    };
    let session_id = resolve_owned_session_id(ctx, &r.meeting_key)?;
    // Wczesna walidacja statusu: CHECK constraint w DB ogranicza sie do trzech
    // wartosci, ale filtr wchodzi w WHERE — niepoprawna wartosc zwrocilaby
    // pusta liste zamiast bledu. Zwracamy 400 dla czytelnego feedbacku GUI.
    if let Some(s) = r.status_filter.as_deref() {
        if !matches!(s, "pending" | "done" | "cancelled") {
            return Err(bad_request("status_filter musi byc pending/done/cancelled"));
        }
    }
    let rows = repository::transcripts::list_action_items_for_meeting(
        &ctx.state.db,
        session_id,
        r.status_filter.as_deref(),
    )
    .map_err(internal)?;
    let items = rows
        .into_iter()
        .map(|a| MeetingActionItemItem {
            id: a.id,
            owner: a.owner,
            task: a.task,
            deadline: a.deadline,
            status: a.status,
            created_at: a.created_at,
            updated_at: a.updated_at,
        })
        .collect();
    Ok(MessageBody::MeetingBody(MeetingPayload::ResActionItemsList(
        MeetingActionItemsListResponse { items },
    )))
}

// =============================================================================
// 12. Action item status update
// =============================================================================

#[handler(variant = "MeetingActionItemStatusUpdateRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn meeting_action_item_status_update(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = meeting_payload(req)?;
    let MeetingPayload::ReqActionItemStatusUpdate(r) = payload else {
        return Err(bad_request("expected ReqActionItemStatusUpdate"));
    };
    if !matches!(r.status.as_str(), "pending" | "done" | "cancelled") {
        return Err(bad_request("status musi byc pending/done/cancelled"));
    }
    // Ownership: sprawdzamy przez join item -> session -> owner. Bezposredniego
    // helpera nie ma, wiec czytamy session_id z wiersza i delegujemy do
    // resolve_owned_session_id po meeting_key. Jesli item nie istnieje -> 404
    // (zgodne z update zwracajacym 0 rows).
    let session_id = {
        let conn = ctx.state.db.lock().unwrap();
        conn.query_row::<i64, _, _>(
            "SELECT session_id FROM meeting_action_items WHERE id = ?1",
            rusqlite::params![r.item_id],
            |row| row.get(0),
        )
        .ok()
    };
    let session_id = session_id
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "action item not found"))?;
    let meeting_key: String = {
        let conn = ctx.state.db.lock().unwrap();
        conn.query_row(
            "SELECT meeting_key FROM meeting_sessions WHERE id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .map_err(internal)?
    };
    // Ta sama sciezka ACL co przy list/export — admin albo owner sesji.
    resolve_owned_session_id(ctx, &meeting_key)?;

    let ok = repository::transcripts::update_action_item_status(&ctx.state.db, r.item_id, &r.status)
        .map_err(internal)?;
    Ok(MessageBody::MeetingBody(
        MeetingPayload::ResActionItemStatusUpdate(MeetingActionItemStatusUpdateResponse {
            success: ok,
        }),
    ))
}

// =============================================================================
// 13. Transcript export (plain text z naglowkiem)
// =============================================================================

fn format_ts_ms(ms: i64) -> String {
    // Zamiana epoch-ms na "YYYY-MM-DD HH:MM:SS" w UTC. Celowo bez strefy —
    // transkrypt jest artefaktem sesji, nie wiadomoscia wyslana o tej godzinie
    // do usera; dodawanie TZ mylaco.
    let secs = ms / 1000;
    let naive = chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
        .unwrap_or_else(chrono::Utc::now);
    naive.format("%Y-%m-%d %H:%M:%S").to_string()
}

#[handler(variant = "MeetingTranscriptExportRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn meeting_transcript_export(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = meeting_payload(req)?;
    let MeetingPayload::ReqTranscriptExport(r) = payload else {
        return Err(bad_request("expected ReqTranscriptExport"));
    };
    let session_id = resolve_owned_session_id(ctx, &r.meeting_key)?;

    let session = repository::transcripts::get_session(&ctx.state.db, session_id)
        .map_err(internal)?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "session not found"))?;
    let entries = repository::transcripts::list_transcripts(&ctx.state.db, session_id)
        .map_err(internal)?;

    // Naglowek: tytul (fallback na meeting_key), start, unikalni uczestnicy.
    let title = session
        .title
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| session.meeting_key.clone());
    let mut participants: Vec<String> = {
        let mut seen = std::collections::BTreeSet::new();
        for e in &entries {
            seen.insert(e.speaker.clone());
        }
        seen.into_iter().collect()
    };
    // Deterministyczna kolejnosc juz z BTreeSet; na pustke zostaw "-".
    if participants.is_empty() {
        participants.push("-".into());
    }

    let mut out = String::with_capacity(256 + entries.len() * 80);
    out.push_str(&format!("Transkrypt spotkania: {}\n", title));
    out.push_str(&format!("Rozpoczęcie: {}\n", session.started_at));
    out.push_str(&format!("Uczestnicy: {}\n", participants.join(", ")));
    out.push_str("================================\n\n");
    for e in &entries {
        out.push_str(&format!(
            "[{}] {}: {}\n",
            format_ts_ms(e.timestamp_ms),
            e.speaker,
            e.text
        ));
    }

    Ok(MessageBody::MeetingBody(MeetingPayload::ResTranscriptExport(
        MeetingTranscriptExportResponse { content: out },
    )))
}

// =============================================================================
// Wake-words CRUD (1 sub-action: list/create/toggle/delete). Pojedynczy
// handler bo limit 256 wariantow MessageBody — caller robi router-side
// dispatch przez `WakeWordOp` enum. Wynik to zawsze pelna lista (klient nie
// musi robic refetch po mutacji).
// =============================================================================

#[handler(variant = "MeetingWakeWordRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn meeting_wake_word(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = meeting_payload(req)?;
    let MeetingPayload::ReqWakeWord(r) = payload else {
        return Err(bad_request("expected ReqWakeWord"));
    };
    use tentaflow_protocol::WakeWordOp;
    use crate::db::repository;
    match &r.op {
        WakeWordOp::List => {}
        WakeWordOp::Create { word } => {
            let trimmed = word.trim();
            if trimmed.is_empty() || trimmed.len() > 64 {
                return Err(bad_request("wake word puste lub za dlugie (max 64)"));
            }
            if trimmed.contains(',') {
                return Err(bad_request("przecinek niedozwolony (separator CSV)"));
            }
            repository::add_wake_word(&ctx.state.db, trimmed).map_err(internal)?;
        }
        WakeWordOp::Toggle { id, enabled } => {
            repository::set_wake_word_enabled(&ctx.state.db, *id, *enabled)
                .map_err(internal)?;
        }
        WakeWordOp::Delete { id } => {
            repository::delete_wake_word(&ctx.state.db, *id).map_err(internal)?;
        }
    }
    let words = repository::list_wake_words(&ctx.state.db).map_err(internal)?;
    let proto_words: Vec<tentaflow_protocol::WakeWord> = words
        .into_iter()
        .map(|w| tentaflow_protocol::WakeWord {
            id: w.id,
            word: w.word,
            enabled: w.enabled,
            created_at: w.created_at,
        })
        .collect();
    Ok(MessageBody::MeetingBody(MeetingPayload::ResWakeWord(
        tentaflow_protocol::MeetingWakeWordResponse {
            words: proto_words,
        },
    )))
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repository::transcripts as repo_tx;
    use crate::dispatch::state::AppState;
    use tentaflow_protocol::{
        MeetingActionItemStatusUpdateRequest, MeetingActionItemsListRequest,
        MeetingSummariesListRequest, MeetingTranscriptExportRequest,
    };

    fn user_ctx(state: std::sync::Arc<AppState>, uid: i64, admin: bool) -> HandlerContext {
        let mut bytes = [0u8; 16];
        bytes[0] = 0xFF;
        bytes[8..].copy_from_slice(&(uid as u64).to_le_bytes());
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: bytes,
                role: if admin { Some("admin".into()) } else { None },
            },
            correlation_id: 1,
            resume_secret: None,
            state,
        }
    }

    /// Tworzy sesje + ustawia owner_user_id. Zwraca (session_id, meeting_key).
    fn make_owned_session(state: &AppState, key: &str, owner: i64) -> i64 {
        let sid = repo_tx::get_or_create_session(&state.db, key, Some("u"), Some("Stand-up 22.04"))
            .expect("create session");
        {
            let conn = state.db.lock().unwrap();
            conn.execute(
                "UPDATE meeting_sessions SET owner_user_id = ?1, started_at = '2024-04-23 14:22:00' WHERE id = ?2",
                rusqlite::params![owner, sid],
            )
            .unwrap();
        }
        sid
    }

    // ----- summaries_list ------------------------------------------------

    #[test]
    fn summaries_list_returns_owner_records_desc() {
        let state = AppState::for_test();
        let sid = make_owned_session(&state, "m-sum", 7);
        repository::transcripts::insert_meeting_summary(&state.db, sid, "D1", "S1", "qwen").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        repository::transcripts::insert_meeting_summary(&state.db, sid, "D2", "S2", "qwen").unwrap();

        let ctx = user_ctx(state.clone(), 7, false);
        let req = MessageBody::MeetingBody(MeetingPayload::ReqSummariesList(
            MeetingSummariesListRequest {
                meeting_key: "m-sum".into(),
                limit: None,
            },
        ));
        let res = meeting_summaries_list(&req, &ctx).expect("ok");
        let MessageBody::MeetingBody(MeetingPayload::ResSummariesList(r)) = res else {
            panic!("wrong variant");
        };
        assert_eq!(r.items.len(), 2);
        assert_eq!(r.items[0].summary_text, "S2");
    }

    #[test]
    fn summaries_list_forbidden_for_non_owner() {
        let state = AppState::for_test();
        let sid = make_owned_session(&state, "m-sum-priv", 7);
        repository::transcripts::insert_meeting_summary(&state.db, sid, "d", "s", "m").unwrap();

        let ctx = user_ctx(state.clone(), 8, false);
        let req = MessageBody::MeetingBody(MeetingPayload::ReqSummariesList(
            MeetingSummariesListRequest {
                meeting_key: "m-sum-priv".into(),
                limit: None,
            },
        ));
        let err = meeting_summaries_list(&req, &ctx).expect_err("must be forbidden");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    }

    #[test]
    fn summaries_list_not_found_for_unknown_key() {
        let state = AppState::for_test();
        let ctx = user_ctx(state.clone(), 1, false);
        let req = MessageBody::MeetingBody(MeetingPayload::ReqSummariesList(
            MeetingSummariesListRequest {
                meeting_key: "nope".into(),
                limit: Some(5),
            },
        ));
        let err = meeting_summaries_list(&req, &ctx).expect_err("nf");
        assert_eq!(err.code, ProtocolErrorCode::NotFound);
    }

    #[test]
    fn summaries_list_empty_ok() {
        let state = AppState::for_test();
        let _ = make_owned_session(&state, "m-empty", 2);
        let ctx = user_ctx(state.clone(), 2, false);
        let req = MessageBody::MeetingBody(MeetingPayload::ReqSummariesList(
            MeetingSummariesListRequest {
                meeting_key: "m-empty".into(),
                limit: None,
            },
        ));
        let res = meeting_summaries_list(&req, &ctx).expect("ok");
        let MessageBody::MeetingBody(MeetingPayload::ResSummariesList(r)) = res else {
            panic!()
        };
        assert!(r.items.is_empty());
    }

    // ----- action_items_list ---------------------------------------------

    #[test]
    fn action_items_list_filters_by_status() {
        let state = AppState::for_test();
        let sid = make_owned_session(&state, "m-ai", 3);
        let a = repository::transcripts::upsert_meeting_action_item(&state.db, sid, "Alice", "task A", None)
            .unwrap();
        repository::transcripts::upsert_meeting_action_item(&state.db, sid, "Bob", "task B", None).unwrap();
        repository::transcripts::update_action_item_status(&state.db, a, "done").unwrap();

        let ctx = user_ctx(state.clone(), 3, false);
        let req = MessageBody::MeetingBody(MeetingPayload::ReqActionItemsList(
            MeetingActionItemsListRequest {
                meeting_key: "m-ai".into(),
                status_filter: Some("pending".into()),
            },
        ));
        let res = meeting_action_items_list(&req, &ctx).unwrap();
        let MessageBody::MeetingBody(MeetingPayload::ResActionItemsList(r)) = res else {
            panic!()
        };
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.items[0].task, "task B");
    }

    #[test]
    fn action_items_list_invalid_status_filter_rejected() {
        let state = AppState::for_test();
        make_owned_session(&state, "m-ai-bad", 1);
        let ctx = user_ctx(state.clone(), 1, false);
        let req = MessageBody::MeetingBody(MeetingPayload::ReqActionItemsList(
            MeetingActionItemsListRequest {
                meeting_key: "m-ai-bad".into(),
                status_filter: Some("bogus".into()),
            },
        ));
        let err = meeting_action_items_list(&req, &ctx).expect_err("bad status");
        assert_eq!(err.code, ProtocolErrorCode::InvalidFrame);
    }

    #[test]
    fn action_items_list_admin_can_see_other_user_session() {
        let state = AppState::for_test();
        let sid = make_owned_session(&state, "m-ai-adm", 99);
        repository::transcripts::upsert_meeting_action_item(&state.db, sid, "X", "t", None).unwrap();
        let ctx = user_ctx(state.clone(), 1, true);
        let req = MessageBody::MeetingBody(MeetingPayload::ReqActionItemsList(
            MeetingActionItemsListRequest {
                meeting_key: "m-ai-adm".into(),
                status_filter: None,
            },
        ));
        let res = meeting_action_items_list(&req, &ctx).unwrap();
        let MessageBody::MeetingBody(MeetingPayload::ResActionItemsList(r)) = res else {
            panic!()
        };
        assert_eq!(r.items.len(), 1);
    }

    // ----- action_item_status_update -------------------------------------

    #[test]
    fn action_item_status_update_transitions_pending_to_done() {
        let state = AppState::for_test();
        let sid = make_owned_session(&state, "m-upd", 5);
        let id = repository::transcripts::upsert_meeting_action_item(&state.db, sid, "A", "t", None).unwrap();

        let ctx = user_ctx(state.clone(), 5, false);
        let req = MessageBody::MeetingBody(MeetingPayload::ReqActionItemStatusUpdate(
            MeetingActionItemStatusUpdateRequest {
                item_id: id,
                status: "done".into(),
            },
        ));
        let res = meeting_action_item_status_update(&req, &ctx).unwrap();
        let MessageBody::MeetingBody(MeetingPayload::ResActionItemStatusUpdate(r)) = res else {
            panic!()
        };
        assert!(r.success);

        let rows =
            repository::transcripts::list_action_items_for_meeting(&state.db, sid, Some("done")).unwrap();
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn action_item_status_update_invalid_status_rejected() {
        let state = AppState::for_test();
        let sid = make_owned_session(&state, "m-upd-bad", 5);
        let id = repository::transcripts::upsert_meeting_action_item(&state.db, sid, "A", "t", None).unwrap();

        let ctx = user_ctx(state.clone(), 5, false);
        let req = MessageBody::MeetingBody(MeetingPayload::ReqActionItemStatusUpdate(
            MeetingActionItemStatusUpdateRequest {
                item_id: id,
                status: "archived".into(),
            },
        ));
        let err = meeting_action_item_status_update(&req, &ctx).expect_err("bad");
        assert_eq!(err.code, ProtocolErrorCode::InvalidFrame);
    }

    #[test]
    fn action_item_status_update_forbidden_for_non_owner() {
        let state = AppState::for_test();
        let sid = make_owned_session(&state, "m-upd-priv", 5);
        let id = repository::transcripts::upsert_meeting_action_item(&state.db, sid, "A", "t", None).unwrap();

        let ctx = user_ctx(state.clone(), 999, false);
        let req = MessageBody::MeetingBody(MeetingPayload::ReqActionItemStatusUpdate(
            MeetingActionItemStatusUpdateRequest {
                item_id: id,
                status: "done".into(),
            },
        ));
        let err = meeting_action_item_status_update(&req, &ctx).expect_err("forbidden");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    }

    #[test]
    fn action_item_status_update_not_found() {
        let state = AppState::for_test();
        let ctx = user_ctx(state.clone(), 1, false);
        let req = MessageBody::MeetingBody(MeetingPayload::ReqActionItemStatusUpdate(
            MeetingActionItemStatusUpdateRequest {
                item_id: 4242,
                status: "done".into(),
            },
        ));
        let err = meeting_action_item_status_update(&req, &ctx).expect_err("nf");
        assert_eq!(err.code, ProtocolErrorCode::NotFound);
    }

    // ----- transcript_export ---------------------------------------------

    #[test]
    fn transcript_export_formats_with_header_and_timestamps() {
        let state = AppState::for_test();
        let sid = make_owned_session(&state, "m-exp", 10);
        // Wstawiamy dwa wpisy przez insert_transcript (uzywa TranscriptEntry z routing::transcript_store).
        use crate::routing::transcript_store::TranscriptEntry;
        let e1 = TranscriptEntry {
            timestamp_ms: 1_713_881_323_000u64, // 2024-04-23 14:08:43 UTC (przyklad)
            speaker: "Maja K.".into(),
            profile_id: None,
            confidence: None,
            is_enrolled: false,
            meeting_id: None,
            text: "Cześć wszystkim.".into(),
            model: "whisper".into(),
        };
        let e2 = TranscriptEntry {
            timestamp_ms: 1_713_881_334_000u64,
            speaker: "Tomek P.".into(),
            profile_id: None,
            confidence: None,
            is_enrolled: false,
            meeting_id: None,
            text: "Skończyłem migrację.".into(),
            model: "whisper".into(),
        };
        repo_tx::insert_transcript(&state.db, sid, &e1).unwrap();
        repo_tx::insert_transcript(&state.db, sid, &e2).unwrap();

        let ctx = user_ctx(state.clone(), 10, false);
        let req = MessageBody::MeetingBody(MeetingPayload::ReqTranscriptExport(
            MeetingTranscriptExportRequest {
                meeting_key: "m-exp".into(),
            },
        ));
        let res = meeting_transcript_export(&req, &ctx).unwrap();
        let MessageBody::MeetingBody(MeetingPayload::ResTranscriptExport(r)) = res else {
            panic!()
        };
        assert!(r.content.starts_with("Transkrypt spotkania: Stand-up 22.04\n"));
        assert!(r.content.contains("Rozpoczęcie: 2024-04-23 14:22:00"));
        assert!(r.content.contains("Uczestnicy: Maja K., Tomek P."));
        assert!(r.content.contains("================================"));
        assert!(r.content.contains("] Maja K.: Cześć wszystkim."));
        assert!(r.content.contains("] Tomek P.: Skończyłem migrację."));
    }

    #[test]
    fn transcript_export_forbidden_for_non_owner() {
        let state = AppState::for_test();
        make_owned_session(&state, "m-exp-priv", 10);
        let ctx = user_ctx(state.clone(), 11, false);
        let req = MessageBody::MeetingBody(MeetingPayload::ReqTranscriptExport(
            MeetingTranscriptExportRequest {
                meeting_key: "m-exp-priv".into(),
            },
        ));
        let err = meeting_transcript_export(&req, &ctx).expect_err("forbidden");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    }
}
