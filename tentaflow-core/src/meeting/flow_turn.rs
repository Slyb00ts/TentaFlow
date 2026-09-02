// ===== File: meeting/flow_turn.rs — one Meeting Bot turn through the flow engine =====
//
// The bot sends a speech segment (`ModelPayload::FlowInvoke`) over its reverse
// QUIC stream; Core builds the envelope (audio payload + meeting context as
// `input_0`), runs the session's flow (stt → combine → llm → tts) and streams
// `Transcript` / `TextDelta` / `AudioChunk` / `Done` back. The bot never picks
// models or flows — everything is resolved from the session row.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::BoxStream;
use futures::StreamExt;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use tentaflow_protocol::{
    ErrorInfo, ErrorType, FlowInvokePayload, ModelStreamChunk, StreamChunkType,
};

use crate::db::repository::transcripts::{self as sessions_repo, SessionRow};
use crate::db::seed::MEETING_BOT_FLOW_ID;
use crate::db::DbPool;
use crate::flow_engine::dispatcher::{DispatchError, FlowActor, FlowOrigin, FlowRequestMeta};
use crate::flow_engine::envelope::{ArtifactProvenance, EnvelopeDelta, FlowEnvelope, FlowValue};
use crate::meeting::manager::{DEFAULT_LLM_ALIAS, DEFAULT_STT_ALIAS, DEFAULT_TTS_ALIAS};
use crate::routing::transcript_store::{self, TranscriptBuilder};
use crate::routing::Router;

/// Identity of the sidecar that opened the reverse stream, as registered in
/// `ServiceManager::register_meeting_bot` (`meeting-bot-<session_id>`).
#[derive(Debug, Clone)]
pub struct ReverseCaller {
    pub service_name: String,
}

/// Hard ceiling for one segment. The bot caps speech far below it (15 s of
/// 16 kHz i16 mono is 480 KiB), so anything reaching 4 MiB is a broken or
/// hostile sender, not speech.
pub const MAX_TURN_AUDIO_BYTES: usize = 4 * 1024 * 1024;
/// The LLM's "stay silent" convention (see the factory Meeting Bot prompt).
const NO_RESPONSE_MARKER: &str = "<NO_RESPONSE>";
/// Text held back before deciding whether the answer starts with the marker;
/// the marker is 13 bytes, tokenizers may split it, 32 covers every split.
const EARLY_GUARD_MAX: usize = 32;
/// Ceiling on TTS bytes held back while the guard window is undecided. At
/// 16 kHz 16-bit mono this is ~16 s of speech — far more audio than the first
/// EARLY_GUARD_MAX characters of text can justify, so crossing it means the
/// flow is misbehaving and the turn is aborted instead of buffering forever.
const HELD_AUDIO_MAX_BYTES: usize = 512 * 1024;
/// How many previous utterances of the meeting go into the LLM context.
const CONTEXT_TRANSCRIPT_ENTRIES: usize = 8;
/// Budget of a whole turn (STT + LLM + TTS); the bot times out at 30 s for
/// the transcript alone, so this only bounds a wedged backend.
const TURN_DEADLINE: Duration = Duration::from_secs(120);

/// Result of draining the flow stream into protocol chunks.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PumpOutcome {
    /// Nothing was forwarded and the execution was cancelled.
    Silenced(SilenceReason),
    /// Text/audio were forwarded; counts are for logs and tests.
    Completed {
        text_chunks: usize,
        audio_chunks: usize,
    },
    Failed(String),
}

/// Why a turn produced no answer. The two cases look identical on the wire but
/// mean different things operationally: the marker is the prompt working as
/// designed, an empty answer is a model or backend that said nothing.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum SilenceReason {
    /// The model emitted the `<NO_RESPONSE>` marker.
    Marker,
    /// The model produced no text at all.
    NoText,
}

impl SilenceReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SilenceReason::Marker => "no_response_marker",
            SilenceReason::NoText => "empty_answer",
        }
    }
}

/// Meeting context the bot attaches to the turn (everything optional).
#[derive(Debug, Default, Clone)]
pub(crate) struct TurnContext {
    pub active_speaker: Option<String>,
    pub roster_json: Option<String>,
    pub hint: Option<String>,
}

/// Entry point from `dispatch_reverse_stream_request`.
pub async fn run_flow_turn(
    router: &Router,
    request_id: String,
    payload: FlowInvokePayload,
    caller: Option<&ReverseCaller>,
    tx: &UnboundedSender<Vec<u8>>,
) {
    let emit = |chunk: StreamChunkType| {
        send_chunk(tx, &request_id, chunk);
    };

    let Some(caller) = caller else {
        emit(error_chunk(
            ErrorType::InvalidRequest,
            "FlowInvoke is accepted only from a registered sidecar service",
        ));
        return;
    };
    let Some(db) = router.db.clone() else {
        emit(error_chunk(
            ErrorType::InternalError,
            "FlowInvoke: router without DB",
        ));
        return;
    };
    // Platform contract: an entry point that bypasses dispatch checks the
    // instance itself. Meeting Bot drains on disable — a turn of a RUNNING
    // session must keep working after the toggle (only SessionStart refuses),
    // so this refuses solely the uninstalled case.
    if !crate::dispatch::app_gate::package_instance_installed(&db, "meeting-bot") {
        emit(error_chunk(
            ErrorType::InvalidRequest,
            "FlowInvoke: meeting-bot application is not installed",
        ));
        return;
    }
    let Some(dispatcher) = router.flow_dispatcher().cloned() else {
        emit(error_chunk(
            ErrorType::InternalError,
            "FlowInvoke: flow dispatcher not wired",
        ));
        return;
    };

    let meta_get = |key: &str| -> Option<String> {
        payload
            .meta
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .filter(|v| !v.trim().is_empty())
    };
    let Some(meeting_id) = meta_get("meeting_id").or_else(|| payload.session_id.clone()) else {
        emit(error_chunk(
            ErrorType::InvalidRequest,
            "FlowInvoke: meeting_id is required",
        ));
        return;
    };
    let session = match lookup_owned_session(&db, &meeting_id, caller) {
        Ok(s) => s,
        Err(msg) => {
            emit(error_chunk(
                ErrorType::InvalidRequest,
                &format!("FlowInvoke: {msg}"),
            ));
            return;
        }
    };
    // Explicit parse: a typo ("yes", "1") must not silently mean "answer in
    // the meeting" — the bot always sends the canonical spelling.
    let respond = match meta_get("respond").as_deref() {
        None | Some("true") => true,
        Some("false") => false,
        Some(other) => {
            emit(error_chunk(
                ErrorType::InvalidRequest,
                &format!("FlowInvoke: meta 'respond' must be 'true' or 'false', got '{other}'"),
            ));
            return;
        }
    };

    if payload.audio.is_none() && payload.text.as_deref().unwrap_or("").trim().is_empty() {
        emit(error_chunk(
            ErrorType::InvalidRequest,
            "FlowInvoke: neither audio nor text given",
        ));
        return;
    }
    if let Some(audio) = payload.audio.as_ref() {
        if audio.audio_data.len() > MAX_TURN_AUDIO_BYTES {
            emit(error_chunk(
                ErrorType::InvalidRequest,
                &format!(
                    "FlowInvoke: audio {} bytes exceeds limit {}",
                    audio.audio_data.len(),
                    MAX_TURN_AUDIO_BYTES
                ),
            ));
            return;
        }
    }

    let pipeline = SessionPipeline::from_row(&session);
    let flow_id = payload
        .flow_id
        .clone()
        .filter(|f| !f.trim().is_empty())
        .unwrap_or_else(|| pipeline.flow_id.clone());
    let language = payload
        .audio
        .as_ref()
        .and_then(|a| a.language.clone())
        .or_else(|| meta_get("language"));
    let context = TurnContext {
        active_speaker: meta_get("active_speaker"),
        roster_json: meta_get("roster"),
        hint: payload.text.clone().filter(|t| !t.trim().is_empty()),
    };

    // Diarization runs on the raw segment concurrently with the flow (it is
    // CPU-bound and independent of the transcript); joined after the
    // transcript is known, exactly like the plain STT reverse path.
    let diarization = spawn_diarization(
        router.db.clone(),
        payload.audio.as_ref().map(|a| a.audio_data.as_slice()),
        &meeting_id,
    );

    let mut envelope = match build_turn_envelope(
        dispatcher.blobs(),
        payload
            .audio
            .as_ref()
            .map(|a| (a.audio_data.clone(), a.mime.clone(), a.sample_rate)),
        &context,
        &meeting_id,
    )
    .await
    {
        Ok(e) => e,
        Err(e) => {
            emit(error_chunk(
                ErrorType::InternalError,
                &format!("FlowInvoke: {e}"),
            ));
            return;
        }
    };
    apply_pipeline_meta(
        &mut envelope,
        &pipeline,
        &meeting_id,
        language.as_deref(),
        respond,
    );

    // §2.5 — a person invited the bot to this meeting, and the session row
    // records who. The bot cannot claim it: `lookup_owned_session` already tied
    // this turn to the row whose container name matches the registered service,
    // so the owner is resolved server-side and never taken from the payload. A
    // row predating the owner column has nobody to name, and naming the meeting
    // is the honest answer there rather than inventing a user.
    let actor = match session.owner_user_id.as_deref() {
        Some(owner) => FlowActor::user(owner),
        None => FlowActor::system_component(meeting_id.clone()),
    };
    let mut req_meta = FlowRequestMeta::new(request_id.clone(), FlowOrigin::Meeting, actor);
    req_meta.user_id = session.owner_user_id.clone();
    req_meta.session_id = Some(meeting_id.clone());
    req_meta.deadline = Some(Instant::now() + TURN_DEADLINE);
    let cancel = req_meta.cancel_token.clone();
    // Armed for the whole turn: if the reverse stream breaks, the listener
    // aborts this task and the guard's Drop cancels the flow. Without it a
    // torn-down consumer would leave LLM + TTS grinding until TURN_DEADLINE.
    let cancel_guard = cancel.clone().drop_guard();

    let exec = match dispatcher
        .dispatch_by_flow_id_streaming(flow_id.clone(), envelope, req_meta)
        .await
    {
        Ok(exec) => exec,
        Err(e) => {
            emit(dispatch_error_chunk(&e));
            return;
        }
    };
    tokio::spawn(async move {
        match exec.outcome.await {
            Ok(o) => info!(
                target: "meeting::flow_turn",
                latency_ms = o.total_latency_ms,
                error = ?o.error,
                "meeting turn completed"
            ),
            Err(_) => warn!(target: "meeting::flow_turn", "meeting turn finalizer dropped"),
        }
    });

    if payload.audio.is_some() {
        let transcript = exec
            .producer_input
            .meta
            .get("stt_transcript")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string();
        if transcript.is_empty() {
            cancel.cancel();
            drop(exec.stream);
            emit(StreamChunkType::Done {
                final_metrics: None,
            });
            return;
        }
        record_transcript(&meeting_id, &transcript, &pipeline.stt_alias, diarization).await;
        emit(StreamChunkType::Transcript(transcript));
    }

    if !respond {
        cancel.cancel();
        drop(exec.stream);
        emit(StreamChunkType::Done {
            final_metrics: None,
        });
        return;
    }

    match pump_stream(exec.stream, cancel, &emit).await {
        PumpOutcome::Silenced(reason) => {
            info!(
                target: "meeting::flow_turn",
                meeting_id = %meeting_id,
                reason = reason.as_str(),
                "bot stays silent"
            );
            emit(StreamChunkType::Done {
                final_metrics: None,
            });
        }
        PumpOutcome::Completed {
            text_chunks,
            audio_chunks,
        } => {
            // The stream ended on its own, so the flow is past its last node;
            // cancelling now would only mark a finished execution as aborted.
            cancel_guard.disarm();
            info!(
                target: "meeting::flow_turn",
                meeting_id = %meeting_id,
                text_chunks,
                audio_chunks,
                "meeting turn streamed"
            );
            emit(StreamChunkType::Done {
                final_metrics: None,
            });
        }
        PumpOutcome::Failed(msg) => {
            warn!(target: "meeting::flow_turn", meeting_id = %meeting_id, "meeting turn failed: {msg}");
            emit(error_chunk(ErrorType::InternalError, &msg));
        }
    }
}

/// Effective models/flow of a session. Rows older than migration 131 carry
/// NULLs and fall back to the same defaults `start_session` would have used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionPipeline {
    pub stt_alias: String,
    pub llm_alias: String,
    pub tts_alias: String,
    pub flow_id: String,
}

impl SessionPipeline {
    pub(crate) fn from_row(row: &SessionRow) -> Self {
        let pick = |v: &Option<String>, default: &str| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or(default)
                .to_string()
        };
        Self {
            stt_alias: pick(&row.stt_alias, DEFAULT_STT_ALIAS),
            llm_alias: pick(&row.llm_alias, DEFAULT_LLM_ALIAS),
            tts_alias: pick(&row.tts_alias, DEFAULT_TTS_ALIAS),
            flow_id: pick(&row.flow_id, MEETING_BOT_FLOW_ID),
        }
    }
}

/// The single ownership gate for every sidecar-initiated request that names a
/// meeting (flow turns, STT audio, meeting events): the session must exist, be
/// live, and its container name recorded at spawn must equal the service name
/// the reverse listener was registered under — so a bot cannot drive another
/// meeting. The SQLite error text never leaves the process: a caller that can
/// probe storage failures learns about our schema, so it only sees "session
/// lookup failed" while the detail goes to the log.
pub(crate) fn lookup_owned_session(
    db: &DbPool,
    meeting_id: &str,
    caller: &ReverseCaller,
) -> Result<SessionRow, String> {
    let row = sessions_repo::get_session_by_meeting_key(db, meeting_id)
        .map_err(|e| {
            warn!(
                target: "meeting::flow_turn",
                service = %caller.service_name,
                "session lookup failed: {e}"
            );
            "session lookup failed".to_string()
        })?
        .ok_or_else(|| format!("unknown meeting_id '{meeting_id}'"))?;
    if row.container_name.as_deref() != Some(caller.service_name.as_str()) {
        return Err(format!(
            "meeting '{meeting_id}' does not belong to service '{}'",
            caller.service_name
        ));
    }
    // A live session sits in 'joining' (set at spawn) and only leaves it for a
    // terminal state, so the gate names the terminal states instead of an
    // "active" one that no writer ever sets.
    if row.ended_at.is_some() || matches!(row.status.as_str(), "ended" | "leaving") {
        return Err(format!("meeting '{meeting_id}' is no longer active"));
    }
    Ok(row)
}

/// Audio payload (raw PCM is wrapped into WAV so every STT backend can decode
/// it) plus the meeting context as `input_0` — the trigger's text port, read
/// by `combine` together with the STT output.
pub(crate) async fn build_turn_envelope(
    blobs: Arc<dyn crate::flow_engine::blob_store::BlobStore>,
    audio: Option<(Vec<u8>, String, Option<u32>)>,
    context: &TurnContext,
    meeting_id: &str,
) -> anyhow::Result<FlowEnvelope> {
    let mut env = FlowEnvelope::empty();
    let context_text = build_context_text(context, meeting_id);

    match audio {
        Some((bytes, mime, sample_rate)) => {
            let (bytes, mime, sample_rate) = if is_raw_pcm(&mime) {
                let rate = sample_rate.unwrap_or(16_000);
                (
                    wrap_pcm16_in_wav(&bytes, rate),
                    "audio/wav".to_string(),
                    Some(rate),
                )
            } else {
                (bytes, mime, sample_rate)
            };
            let blob_ref = blobs.put(bytes, &mime).await?;
            env.payload = FlowValue::Audio {
                blob_ref,
                mime,
                sample_rate,
            };
            env.put_artifact(
                "input_0",
                FlowValue::Text(context_text),
                ArtifactProvenance {
                    producer_node_id: "meeting-bot".into(),
                    producer_node_type: "flow_invoke".into(),
                    timestamp_ms: now_ms(),
                },
            )?;
        }
        None => {
            env.payload = FlowValue::Text(context_text);
        }
    }
    Ok(env)
}

/// Meta contract of the factory flow: models come from the session, audio out
/// only when the bot wants an answer, `format=pcm` so the TTS node emits the
/// raw 16-bit frames the bot's mic injection plays directly.
pub(crate) fn apply_pipeline_meta(
    env: &mut FlowEnvelope,
    pipeline: &SessionPipeline,
    meeting_id: &str,
    language: Option<&str>,
    respond: bool,
) {
    use serde_json::Value;
    env.set_output_audio(respond);
    env.meta
        .insert("model".into(), Value::String(pipeline.llm_alias.clone()));
    env.meta.insert(
        "stt_model".into(),
        Value::String(pipeline.stt_alias.clone()),
    );
    env.meta.insert(
        "tts_model".into(),
        Value::String(pipeline.tts_alias.clone()),
    );
    env.meta
        .insert("meeting_id".into(), Value::String(meeting_id.to_string()));
    env.meta
        .insert("format".into(), Value::String("pcm".into()));
    if let Some(lang) = language.filter(|l| !l.trim().is_empty()) {
        env.meta
            .insert("language".into(), Value::String(lang.to_string()));
    }
}

/// Human-readable meeting state for the LLM: who is speaking, who is present,
/// the last utterances. Kept as plain labelled lines — the flow's own system
/// prompt decides how to use them.
pub(crate) fn build_context_text(context: &TurnContext, meeting_id: &str) -> String {
    let mut lines: Vec<String> = vec!["Meeting context:".to_string()];
    if let Some(speaker) = context.active_speaker.as_deref().map(str::trim) {
        if !speaker.is_empty() {
            lines.push(format!("Active speaker: {speaker}"));
        }
    }
    let participants = roster_names(context.roster_json.as_deref());
    if !participants.is_empty() {
        lines.push(format!("Participants: {}", participants.join(", ")));
    }
    let recent = transcript_store::recent_for_meeting(meeting_id, CONTEXT_TRANSCRIPT_ENTRIES);
    if !recent.is_empty() {
        lines.push("Recent transcript:".to_string());
        for entry in recent {
            lines.push(format!("[{}]: {}", entry.speaker, entry.text));
        }
    }
    if let Some(hint) = context.hint.as_deref().map(str::trim) {
        if !hint.is_empty() {
            lines.push(hint.to_string());
        }
    }
    lines.push("New utterance:".to_string());
    lines.join("\n")
}

/// Names from the bot's roster snapshot (`RosterEntry` JSON array). Anything
/// unparsable yields no participants rather than an error — the roster is
/// context, not input.
fn roster_names(roster_json: Option<&str>) -> Vec<String> {
    let Some(raw) = roster_json else {
        return Vec::new();
    };
    let Ok(serde_json::Value::Array(entries)) = serde_json::from_str::<serde_json::Value>(raw)
    else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|e| {
            e.get("speaker_name")
                .and_then(|v| v.as_str())
                .or_else(|| e.get("speaker_id").and_then(|v| v.as_str()))
                .or_else(|| e.get("name").and_then(|v| v.as_str()))
        })
        .map(|s| {
            s.chars()
                .filter(|c| !c.is_control())
                .take(128)
                .collect::<String>()
        })
        .filter(|s| !s.trim().is_empty())
        .take(64)
        .collect()
}

fn is_raw_pcm(mime: &str) -> bool {
    matches!(
        mime.to_ascii_lowercase().as_str(),
        "audio/pcm" | "audio/l16" | "audio/x-raw" | "pcm"
    )
}

/// 44-byte RIFF header for 16-bit mono little-endian PCM.
pub(crate) fn wrap_pcm16_in_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let data_len = pcm.len() as u32;
    let byte_rate = sample_rate * 2;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// -----------------------------------------------------------------------------
// Transcript persistence shared with the plain STT reverse path
// -----------------------------------------------------------------------------

/// Speaker identification job started before the transcript exists.
pub(crate) struct DiarizationTask {
    #[cfg(feature = "inference-diarization")]
    handle: Option<tokio::task::JoinHandle<Option<crate::diarization::service::IdentifyResult>>>,
}

/// Starts WeSpeaker identification of the segment (CPU-bound, so
/// `spawn_blocking`) when diarization is compiled in and a DB is available.
pub(crate) fn spawn_diarization(
    db: Option<DbPool>,
    audio: Option<&[u8]>,
    meeting_id: &str,
) -> DiarizationTask {
    #[cfg(feature = "inference-diarization")]
    {
        let handle = match (db, audio) {
            (Some(pool), Some(bytes)) => {
                let audio_clone = bytes.to_vec();
                let mid = meeting_id.to_string();
                Some(tokio::task::spawn_blocking(move || {
                    crate::diarization::identify_speaker_with_profiles(&pool, &audio_clone, &mid)
                }))
            }
            _ => None,
        };
        DiarizationTask { handle }
    }
    #[cfg(not(feature = "inference-diarization"))]
    {
        let _ = (db, audio, meeting_id);
        DiarizationTask {}
    }
}

/// Pushes the utterance into the live transcript store (ring buffer + DB),
/// labelled with the identified speaker when diarization produced one.
/// Returns the display speaker. Empty text is never recorded.
pub(crate) async fn record_transcript(
    meeting_id: &str,
    text: &str,
    model: &str,
    diarization: DiarizationTask,
) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    #[allow(unused_mut)]
    let mut builder = TranscriptBuilder::new(text, model).meeting_id(meeting_id);
    #[cfg(feature = "inference-diarization")]
    {
        let ident = match diarization.handle {
            Some(h) => h.await.ok().flatten(),
            None => None,
        };
        if let Some(ident) = ident {
            builder = builder.speaker(ident.label.clone());
            if let Some(pid) = ident.profile_id {
                builder = builder.profile_id(pid);
            }
            if let Some(c) = ident.confidence {
                builder = builder.confidence(c);
            }
        }
    }
    #[cfg(not(feature = "inference-diarization"))]
    let _ = diarization;
    let speaker = builder.speaker.clone();
    let profile_id = builder.profile_id;
    transcript_store::push(builder);
    // Metrics only: the utterance and the speaker's name are personal data of
    // meeting participants and must never reach the log. The diarization
    // profile id (a database key) is the only speaker reference kept here.
    info!(
        target: "meeting::flow_turn",
        model = %model,
        chars = text.chars().count(),
        profile_id = ?profile_id,
        "transcript recorded"
    );
    Some(speaker)
}

// -----------------------------------------------------------------------------
// Stream pump with the <NO_RESPONSE> guard
// -----------------------------------------------------------------------------

/// Forwards flow deltas as protocol chunks. Text and audio are held back
/// until the first `EARLY_GUARD_MAX` bytes of text prove the answer is not
/// the silence marker; on the marker the execution is cancelled and nothing
/// that was held is released (no half-synthesised audio reaches the meeting).
pub(crate) async fn pump_stream(
    mut stream: BoxStream<'static, anyhow::Result<EnvelopeDelta>>,
    cancel: CancellationToken,
    emit: &impl Fn(StreamChunkType),
) -> PumpOutcome {
    let mut decided = false;
    let mut held_text = String::with_capacity(EARLY_GUARD_MAX + 16);
    let mut held_audio: Vec<Vec<u8>> = Vec::new();
    let mut held_audio_bytes = 0usize;
    let mut text_chunks = 0usize;
    let mut audio_chunks = 0usize;

    while let Some(item) = stream.next().await {
        match item {
            Ok(EnvelopeDelta::Llm(chunk)) => {
                if let Some(err) = chunk.error {
                    cancel.cancel();
                    return PumpOutcome::Failed(format!("llm: {err}"));
                }
                if chunk.text_delta.is_empty() {
                    continue;
                }
                if decided {
                    emit(StreamChunkType::TextDelta(chunk.text_delta));
                    text_chunks += 1;
                    continue;
                }
                held_text.push_str(&chunk.text_delta);
                if held_text.contains(NO_RESPONSE_MARKER) {
                    cancel.cancel();
                    return PumpOutcome::Silenced(SilenceReason::Marker);
                }
                if held_text.len() > EARLY_GUARD_MAX {
                    decided = true;
                    emit(StreamChunkType::TextDelta(std::mem::take(&mut held_text)));
                    text_chunks += 1;
                    held_audio_bytes = 0;
                    for pcm in held_audio.drain(..) {
                        emit(StreamChunkType::AudioChunk(pcm));
                        audio_chunks += 1;
                    }
                }
            }
            Ok(EnvelopeDelta::Audio(chunk)) => {
                if chunk.bytes_delta.is_empty() {
                    continue;
                }
                if decided {
                    emit(StreamChunkType::AudioChunk(chunk.bytes_delta));
                    audio_chunks += 1;
                } else {
                    held_audio_bytes += chunk.bytes_delta.len();
                    held_audio.push(chunk.bytes_delta);
                    if held_audio_bytes > HELD_AUDIO_MAX_BYTES {
                        cancel.cancel();
                        return PumpOutcome::Failed(format!(
                            "held TTS audio exceeded {HELD_AUDIO_MAX_BYTES} bytes before the \
                             answer left the guard window"
                        ));
                    }
                }
            }
            Err(e) => {
                cancel.cancel();
                return PumpOutcome::Failed(e.to_string());
            }
        }
    }

    if !decided {
        // Short answer: the whole text fit inside the guard window.
        if held_text.contains(NO_RESPONSE_MARKER) {
            cancel.cancel();
            return PumpOutcome::Silenced(SilenceReason::Marker);
        }
        if held_text.trim().is_empty() {
            cancel.cancel();
            return PumpOutcome::Silenced(SilenceReason::NoText);
        }
        emit(StreamChunkType::TextDelta(std::mem::take(&mut held_text)));
        text_chunks += 1;
        for pcm in held_audio.drain(..) {
            emit(StreamChunkType::AudioChunk(pcm));
            audio_chunks += 1;
        }
    }
    PumpOutcome::Completed {
        text_chunks,
        audio_chunks,
    }
}

/// Maps a dispatcher failure to the protocol error chunk. "No STT service" is
/// forwarded verbatim so the bot log shows the operator-facing message.
pub(crate) fn dispatch_error_chunk(e: &DispatchError) -> StreamChunkType {
    match e {
        DispatchError::SttServiceUnavailable => error_chunk(
            ErrorType::InternalError,
            &crate::error::CoreError::SttServiceUnavailable.to_string(),
        ),
        DispatchError::Denied { .. } => error_chunk(ErrorType::InvalidRequest, &e.to_string()),
        DispatchError::CompileFailed { .. } => {
            error_chunk(ErrorType::InvalidRequest, &e.to_string())
        }
        other => error_chunk(ErrorType::InternalError, &other.to_string()),
    }
}

fn error_chunk(error_type: ErrorType, message: &str) -> StreamChunkType {
    StreamChunkType::Error(ErrorInfo {
        error_type,
        message: message.to_string(),
        details: None,
    })
}

fn send_chunk(tx: &UnboundedSender<Vec<u8>>, request_id: &str, chunk: StreamChunkType) {
    let frame = ModelStreamChunk {
        request_id: request_id.to_string(),
        chunk,
    };
    if let Ok(bytes) = crate::mesh::cbor::encode(&frame) {
        let _ = tx.send(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::blob_store::InMemoryBlobStore;
    use crate::flow_engine::envelope::{AudioStreamChunk, FinishReason, LlmStreamChunk};
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    fn llm(text: &str) -> anyhow::Result<EnvelopeDelta> {
        Ok(EnvelopeDelta::Llm(LlmStreamChunk {
            choice_index: 0,
            text_delta: text.to_string(),
            reasoning_delta: None,
            tool_calls: Vec::new(),
            usage: None,
            perf: None,
            finish_reason: None,
            error: None,
        }))
    }

    fn audio(bytes: &[u8], last: bool) -> anyhow::Result<EnvelopeDelta> {
        Ok(EnvelopeDelta::Audio(AudioStreamChunk {
            choice_index: 0,
            bytes_delta: bytes.to_vec(),
            mime: "audio/pcm".into(),
            sample_rate: Some(16_000),
            finish_reason: if last { Some(FinishReason::Stop) } else { None },
        }))
    }

    fn collect(
        items: Vec<anyhow::Result<EnvelopeDelta>>,
    ) -> (PumpOutcome, Vec<StreamChunkType>, CancellationToken) {
        let out = Arc::new(Mutex::new(Vec::new()));
        let sink = out.clone();
        let cancel = CancellationToken::new();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let outcome = rt.block_on(pump_stream(
            Box::pin(futures::stream::iter(items)),
            cancel.clone(),
            &move |c| sink.lock().unwrap().push(c),
        ));
        let chunks = std::mem::take(&mut *out.lock().unwrap());
        (outcome, chunks, cancel)
    }

    // The marker split across three tokens must still silence the turn: no
    // text, no audio, execution cancelled.
    #[test]
    fn no_response_split_across_tokens_is_silenced() {
        let (outcome, chunks, cancel) = collect(vec![
            llm("<NO"),
            audio(&[1, 2, 3], false),
            llm("_RESP"),
            llm("ONSE>"),
            audio(&[4, 5, 6], true),
        ]);
        assert_eq!(outcome, PumpOutcome::Silenced(SilenceReason::Marker));
        assert!(chunks.is_empty(), "nothing may leak: {chunks:?}");
        assert!(cancel.is_cancelled());
    }

    // A short real answer (below the guard window) is released at end of
    // stream, text before audio.
    #[test]
    fn short_answer_is_flushed_at_end() {
        let (outcome, chunks, cancel) =
            collect(vec![llm("Tak."), audio(&[9, 9], false), audio(&[8], true)]);
        assert_eq!(
            outcome,
            PumpOutcome::Completed {
                text_chunks: 1,
                audio_chunks: 2
            }
        );
        assert!(matches!(&chunks[0], StreamChunkType::TextDelta(t) if t == "Tak."));
        assert!(matches!(&chunks[1], StreamChunkType::AudioChunk(b) if b == &vec![9, 9]));
        assert!(matches!(&chunks[2], StreamChunkType::AudioChunk(b) if b == &vec![8]));
        assert!(!cancel.is_cancelled());
    }

    // Once the guard window is exceeded everything streams through live.
    #[test]
    fn long_answer_streams_after_guard_window() {
        let long = "Odpowiadam na pytanie o status projektu. ";
        let (outcome, chunks, _) = collect(vec![
            llm(long),
            llm("Druga część."),
            audio(&[1], false),
            audio(&[2], true),
        ]);
        assert_eq!(
            outcome,
            PumpOutcome::Completed {
                text_chunks: 2,
                audio_chunks: 2
            }
        );
        assert!(matches!(&chunks[0], StreamChunkType::TextDelta(t) if t == long));
        assert!(matches!(&chunks[1], StreamChunkType::TextDelta(t) if t == "Druga część."));
        assert!(matches!(&chunks[2], StreamChunkType::AudioChunk(_)));
    }

    #[test]
    fn empty_answer_is_silenced() {
        let (outcome, chunks, _) = collect(vec![llm("  "), audio(&[], true)]);
        assert_eq!(outcome, PumpOutcome::Silenced(SilenceReason::NoText));
        assert!(chunks.is_empty());
    }

    #[test]
    fn stream_error_becomes_failed() {
        let (outcome, _, cancel) = collect(vec![llm("Hej"), Err(anyhow::anyhow!("backend down"))]);
        assert_eq!(outcome, PumpOutcome::Failed("backend down".into()));
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn stt_unavailable_maps_to_core_message() {
        let chunk = dispatch_error_chunk(&DispatchError::SttServiceUnavailable);
        match chunk {
            StreamChunkType::Error(e) => assert_eq!(
                e.message,
                crate::error::CoreError::SttServiceUnavailable.to_string()
            ),
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn wav_wrapper_has_valid_header() {
        let pcm = vec![0u8, 1, 2, 3];
        let wav = wrap_pcm16_in_wav(&pcm, 16_000);
        assert_eq!(wav.len(), 44 + 4);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
        assert_eq!(u32::from_le_bytes(wav[28..32].try_into().unwrap()), 32_000);
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 4);
        assert_eq!(&wav[44..], &pcm[..]);
    }

    #[test]
    fn pipeline_falls_back_to_defaults_on_legacy_rows() {
        let row = SessionRow {
            id: 1,
            meeting_key: "mtg-1".into(),
            meeting_url: None,
            title: None,
            started_at: String::new(),
            last_activity_at: String::new(),
            entry_count: 0,
            status: "active".into(),
            ended_at: None,
            container_id: None,
            container_name: Some("meeting-bot-1".into()),
            quic_port: None,
            vnc_port: None,
            novnc_port: None,
            bot_endpoint_id: None,
            platform: None,
            owner_user_id: None,
            lifecycle_stage: None,
            lifecycle_details: None,
            lifecycle_updated_at: None,
            backend_stt_model: None,
            backend_tts_model: None,
            backend_summarization_model: None,
            backend_diarization_model: None,
            backend_streaming_latency_ms: None,
            backend_enrolled_speakers: None,
            backend_total_participants: None,
            stt_alias: None,
            llm_alias: Some("my-llm".into()),
            tts_alias: Some("  ".into()),
            flow_id: None,
        };
        let p = SessionPipeline::from_row(&row);
        assert_eq!(p.stt_alias, DEFAULT_STT_ALIAS);
        assert_eq!(p.llm_alias, "my-llm");
        assert_eq!(p.tts_alias, DEFAULT_TTS_ALIAS);
        assert_eq!(p.flow_id, MEETING_BOT_FLOW_ID);
    }

    #[tokio::test]
    async fn envelope_wraps_pcm_and_carries_context_artifact() {
        let blobs: Arc<dyn crate::flow_engine::blob_store::BlobStore> =
            Arc::new(InMemoryBlobStore::new());
        let ctx = TurnContext {
            active_speaker: Some("Anna".into()),
            roster_json: Some(
                r#"[{"speaker_id":"a","speaker_name":"Anna"},{"speaker_id":"b"}]"#.into(),
            ),
            hint: Some("hint".into()),
        };
        let env = build_turn_envelope(
            blobs.clone(),
            Some((vec![0u8; 320], "audio/pcm".into(), Some(16_000))),
            &ctx,
            "mtg-ctx-test",
        )
        .await
        .unwrap();
        let FlowValue::Audio {
            blob_ref,
            mime,
            sample_rate,
        } = &env.payload
        else {
            panic!("payload must be audio");
        };
        assert_eq!(mime, "audio/wav");
        assert_eq!(*sample_rate, Some(16_000));
        let bytes = blobs.get(blob_ref).await.unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(bytes.len(), 44 + 320);
        let Some(FlowValue::Text(context)) = env.artifacts.get("input_0") else {
            panic!("input_0 must be the text context");
        };
        assert!(context.contains("Active speaker: Anna"));
        assert!(context.contains("Participants: Anna, b"));
        assert!(context.contains("hint"));
        assert!(context.ends_with("New utterance:"));
    }

    // ---- seeded Meeting Bot flow with scripted STT / LLM / TTS ----

    use crate::flow_engine::cache::CompiledFlow;
    use crate::flow_engine::dispatcher::build_registry_for_test;
    use crate::flow_engine::dispatchers::audit::AuditEvent;
    use crate::flow_engine::dispatchers::{
        AuditSink, LlmDispatcher, LlmRequest, LlmResponse, SttDispatcher, SttRequest, SttResponse,
        TtsDispatcher, TtsRequest, TtsResponse, TtsStreamChunk,
    };
    use crate::flow_engine::executor::{execute_streaming, StreamingExecution};
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use async_trait::async_trait;
    use futures::stream::BoxStream;

    type Calls = Arc<Mutex<Vec<&'static str>>>;

    struct ScriptedStt {
        text: String,
        unavailable: bool,
        calls: Calls,
    }
    #[async_trait]
    impl SttDispatcher for ScriptedStt {
        async fn transcribe(&self, req: SttRequest) -> anyhow::Result<SttResponse> {
            self.calls.lock().unwrap().push("stt");
            assert_eq!(
                req.model, "stt-alias",
                "stt model must come from envelope meta"
            );
            assert_eq!(req.audio.mime, "audio/wav");
            if self.unavailable {
                return Err(anyhow::Error::from(
                    crate::error::CoreError::SttServiceUnavailable,
                ));
            }
            Ok(SttResponse {
                text: self.text.clone(),
                ..Default::default()
            })
        }
    }

    struct ScriptedLlm {
        deltas: Vec<&'static str>,
        calls: Calls,
    }
    #[async_trait]
    impl LlmDispatcher for ScriptedLlm {
        async fn execute_chat(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            panic!("meeting flow must stream");
        }
        async fn stream_chat(
            &self,
            req: LlmRequest,
        ) -> anyhow::Result<BoxStream<'static, anyhow::Result<LlmStreamChunk>>> {
            self.calls.lock().unwrap().push("llm");
            assert_eq!(
                req.model, "llm-alias",
                "llm model must come from envelope meta"
            );
            let user = req
                .messages
                .iter()
                .filter_map(|m| m.text())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                user.contains("New utterance:"),
                "context must reach the model: {user}"
            );
            assert!(
                user.contains("status projektu"),
                "transcript must reach the model: {user}"
            );
            let n = self.deltas.len();
            let items: Vec<anyhow::Result<LlmStreamChunk>> = self
                .deltas
                .iter()
                .enumerate()
                .map(|(i, d)| {
                    Ok(LlmStreamChunk {
                        choice_index: 0,
                        text_delta: d.to_string(),
                        reasoning_delta: None,
                        tool_calls: Vec::new(),
                        usage: None,
                        perf: None,
                        finish_reason: (i + 1 == n).then_some(FinishReason::Stop),
                        error: None,
                    })
                })
                .collect();
            Ok(Box::pin(futures::stream::iter(items)))
        }
    }

    struct ScriptedTts {
        calls: Calls,
        blobs: Arc<dyn crate::flow_engine::blob_store::BlobStore>,
    }
    #[async_trait]
    impl TtsDispatcher for ScriptedTts {
        // The tts node synthesises per sentence through the blocking call and
        // chunks the blob itself, so this is the path the seeded flow takes.
        async fn synthesize(&self, req: TtsRequest) -> anyhow::Result<TtsResponse> {
            self.calls.lock().unwrap().push("tts");
            assert_eq!(
                req.model, "tts-alias",
                "tts model must come from envelope meta"
            );
            assert_eq!(req.format.as_deref(), Some("pcm"));
            let audio = self.blobs.put(vec![1, 2, 3, 4, 5, 6], "audio/pcm").await?;
            Ok(TtsResponse {
                audio,
                mime: "audio/pcm".into(),
                sample_rate: Some(16_000),
            })
        }
        async fn stream_synthesize(
            &self,
            req: TtsRequest,
        ) -> anyhow::Result<BoxStream<'static, anyhow::Result<TtsStreamChunk>>> {
            self.calls.lock().unwrap().push("tts");
            assert_eq!(
                req.model, "tts-alias",
                "tts model must come from envelope meta"
            );
            assert_eq!(req.format.as_deref(), Some("pcm"));
            let chunks = vec![
                Ok(TtsStreamChunk {
                    choice_index: 0,
                    bytes_delta: vec![1, 2, 3],
                    mime: "audio/pcm".into(),
                    sample_rate: Some(16_000),
                    finish_reason: None,
                }),
                Ok(TtsStreamChunk {
                    choice_index: 0,
                    bytes_delta: vec![4, 5, 6],
                    mime: "audio/pcm".into(),
                    sample_rate: Some(16_000),
                    finish_reason: Some(FinishReason::Stop),
                }),
            ];
            Ok(Box::pin(futures::stream::iter(chunks)))
        }
    }

    struct NoopAudit;
    #[async_trait]
    impl AuditSink for NoopAudit {
        async fn record(&self, _e: AuditEvent) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn test_db() -> DbPool {
        let tmp = tempfile::NamedTempFile::new().expect("tempfile");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        crate::db::init(&path).expect("init db")
    }

    async fn run_seeded_flow(
        stt_text: &str,
        stt_unavailable: bool,
        llm_deltas: Vec<&'static str>,
    ) -> (anyhow::Result<StreamingExecution>, Calls) {
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let llm: Arc<dyn LlmDispatcher> = Arc::new(ScriptedLlm {
            deltas: llm_deltas,
            calls: calls.clone(),
        });
        let (exec, _cancel) =
            run_seeded_flow_with_llm(stt_text, stt_unavailable, llm, calls.clone()).await;
        (exec, calls)
    }

    /// Same seeded Meeting Bot flow, but the LLM and the execution's cancel
    /// token are supplied by the caller — used by the cancellation test.
    async fn run_seeded_flow_with_llm(
        stt_text: &str,
        stt_unavailable: bool,
        llm: Arc<dyn LlmDispatcher>,
        calls: Calls,
    ) -> (anyhow::Result<StreamingExecution>, CancellationToken) {
        let registry = Arc::new(build_registry_for_test());
        let compiled = Arc::new(
            CompiledFlow::from_json(
                MEETING_BOT_FLOW_ID,
                crate::db::seed::MEETING_BOT_FLOW_JSON,
                &registry,
            )
            .expect("factory Meeting Bot flow compiles"),
        );
        let blobs: Arc<dyn crate::flow_engine::blob_store::BlobStore> =
            Arc::new(InMemoryBlobStore::new());
        let mut env = build_turn_envelope(
            blobs.clone(),
            Some((vec![0u8; 3200], "audio/pcm".into(), Some(16_000))),
            &TurnContext {
                active_speaker: Some("Anna".into()),
                ..Default::default()
            },
            "mtg-seeded",
        )
        .await
        .unwrap();
        let pipeline = SessionPipeline {
            stt_alias: "stt-alias".into(),
            llm_alias: "llm-alias".into(),
            tts_alias: "tts-alias".into(),
            flow_id: MEETING_BOT_FLOW_ID.into(),
        };
        apply_pipeline_meta(&mut env, &pipeline, "mtg-seeded", Some("pl"), true);

        let mut ctx = stub_ctx();
        ctx.blobs = blobs;
        ctx.audit = Arc::new(NoopAudit);
        ctx.stt = Arc::new(ScriptedStt {
            text: stt_text.to_string(),
            unavailable: stt_unavailable,
            calls: calls.clone(),
        });
        ctx.llm = llm;
        ctx.tts = Arc::new(ScriptedTts {
            calls: calls.clone(),
            blobs: ctx.blobs.clone(),
        });
        let cancel = ctx.cancel_token.clone();
        let exec = execute_streaming(test_db(), compiled, env, ctx, registry).await;
        (exec, cancel)
    }

    // The STT node finishes before `execute_streaming` returns: the transcript
    // is readable (and therefore recordable / sendable) before a single LLM
    // token is consumed, and the answer then streams text first, audio after.
    #[tokio::test]
    async fn seeded_flow_exposes_transcript_before_first_token() {
        let (exec, calls) = run_seeded_flow(
            "Jaki jest status projektu?",
            false,
            vec!["Status projektu jest dobry, ", "zamykamy go w piątek."],
        )
        .await;
        let exec = exec.expect("flow starts");
        let transcript = exec
            .producer_input
            .meta
            .get("stt_transcript")
            .and_then(|v| v.as_str())
            .expect("stt_transcript in producer input");
        assert_eq!(transcript, "Jaki jest status projektu?");
        assert_eq!(calls.lock().unwrap().first(), Some(&"stt"));

        let out = Arc::new(Mutex::new(Vec::new()));
        let sink = out.clone();
        let outcome = pump_stream(exec.stream, CancellationToken::new(), &move |c| {
            sink.lock().unwrap().push(c)
        })
        .await;
        let chunks = std::mem::take(&mut *out.lock().unwrap());
        let PumpOutcome::Completed {
            text_chunks,
            audio_chunks,
        } = outcome
        else {
            panic!("expected Completed, got {outcome:?}");
        };
        assert!(text_chunks >= 1, "answer text must stream");
        assert!(audio_chunks >= 1, "TTS audio must reach the bot");
        assert!(matches!(
            chunks.first(),
            Some(StreamChunkType::TextDelta(_))
        ));
        let text: String = chunks
            .iter()
            .filter_map(|c| match c {
                StreamChunkType::TextDelta(t) => Some(t.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "Status projektu jest dobry, zamykamy go w piątek.");
        let audio: Vec<u8> = chunks
            .iter()
            .filter_map(|c| match c {
                StreamChunkType::AudioChunk(b) => Some(b.clone()),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(audio, vec![1, 2, 3, 4, 5, 6]);
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls[0], "stt");
        assert!(calls.contains(&"llm") && calls.contains(&"tts"));
    }

    #[tokio::test]
    async fn seeded_flow_no_response_sends_no_audio() {
        let (exec, _) = run_seeded_flow(
            "Jaki jest status projektu?",
            false,
            vec!["<NO_", "RESPONSE>"],
        )
        .await;
        let exec = exec.expect("flow starts");
        let out = Arc::new(Mutex::new(Vec::new()));
        let sink = out.clone();
        let cancel = CancellationToken::new();
        let outcome = pump_stream(exec.stream, cancel.clone(), &move |c| {
            sink.lock().unwrap().push(c)
        })
        .await;
        assert_eq!(outcome, PumpOutcome::Silenced(SilenceReason::Marker));
        assert!(
            out.lock().unwrap().is_empty(),
            "no text and no audio may leak"
        );
        assert!(cancel.is_cancelled());
    }

    #[tokio::test]
    async fn seeded_flow_without_stt_service_reports_unavailable() {
        let (exec, calls) =
            run_seeded_flow("Jaki jest status projektu?", true, vec!["never"]).await;
        let err = match exec {
            Ok(_) => panic!("flow must fail without an STT service"),
            Err(e) => e,
        };
        let dispatch_err = DispatchError::from(err);
        assert!(matches!(dispatch_err, DispatchError::SttServiceUnavailable));
        match dispatch_error_chunk(&dispatch_err) {
            StreamChunkType::Error(e) => assert_eq!(
                e.message,
                crate::error::CoreError::SttServiceUnavailable.to_string()
            ),
            other => panic!("expected Error chunk, got {other:?}"),
        }
        assert!(!calls.lock().unwrap().contains(&"llm"), "LLM must not run");
    }

    /// LLM whose stream counts how many chunks were actually pulled, so the
    /// test can prove generation stopped instead of running to the end.
    struct CountingLlm {
        pulled: Arc<std::sync::atomic::AtomicUsize>,
        total: usize,
        calls: Calls,
    }

    #[async_trait]
    impl LlmDispatcher for CountingLlm {
        async fn execute_chat(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
            panic!("meeting flow must stream");
        }
        async fn stream_chat(
            &self,
            _req: LlmRequest,
        ) -> anyhow::Result<BoxStream<'static, anyhow::Result<LlmStreamChunk>>> {
            self.calls.lock().unwrap().push("llm");
            let pulled = Arc::clone(&self.pulled);
            let total = self.total;
            Ok(Box::pin(futures::stream::unfold(0usize, move |i| {
                let pulled = Arc::clone(&pulled);
                async move {
                    if i >= total {
                        return None;
                    }
                    pulled.fetch_add(1, Ordering::SeqCst);
                    // Long enough that the answer leaves the guard window on
                    // the first chunk, so every later chunk is a live delta.
                    tokio::task::yield_now().await;
                    Some((
                        Ok(LlmStreamChunk {
                            choice_index: 0,
                            text_delta: "Odpowiadam na pytanie o status projektu. ".to_string(),
                            reasoning_delta: None,
                            tool_calls: Vec::new(),
                            usage: None,
                            perf: None,
                            finish_reason: (i + 1 == total).then_some(FinishReason::Stop),
                            error: None,
                        }),
                        i + 1,
                    ))
                }
            })))
        }
    }

    // CR-001: a consumer that walks away (the reverse stream broke, the
    // listener aborted the turn) must stop the flow. Dropping the stream
    // cancels the execution instead of letting LLM + TTS grind on to the
    // 120 s turn deadline.
    #[tokio::test]
    async fn dropped_consumer_cancels_the_flow() {
        use std::sync::atomic::AtomicUsize;
        let calls: Calls = Arc::new(Mutex::new(Vec::new()));
        let pulled = Arc::new(AtomicUsize::new(0));
        let llm: Arc<dyn LlmDispatcher> = Arc::new(CountingLlm {
            pulled: Arc::clone(&pulled),
            total: 500,
            calls: calls.clone(),
        });
        let (exec, cancel) =
            run_seeded_flow_with_llm("Jaki jest status projektu?", false, llm, calls.clone()).await;
        let exec = exec.expect("flow starts");
        assert!(!cancel.is_cancelled());

        drop(exec.stream);
        let outcome = tokio::time::timeout(Duration::from_secs(10), exec.outcome)
            .await
            .expect("finalizer settles quickly, not at the turn deadline")
            .expect("outcome");

        assert!(
            cancel.is_cancelled(),
            "dropping the consumer must cancel the execution"
        );
        assert_eq!(
            outcome.finish_reason,
            crate::flow_engine::envelope::FinishReason::Cancelled
        );
        let seen = pulled.load(Ordering::SeqCst);
        assert!(
            seen < 500,
            "generation must stop early, pulled {seen} of 500 chunks"
        );
    }

    // ---- migration 131: session pipeline columns ----

    #[test]
    fn session_pipeline_columns_roundtrip() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        crate::db::migrations::run(&conn).unwrap();
        for column in ["stt_alias", "llm_alias", "tts_alias", "flow_id"] {
            let mut stmt = conn.prepare("PRAGMA table_info(meeting_sessions)").unwrap();
            let names: Vec<String> = stmt
                .query_map([], |r| r.get::<_, String>(1))
                .unwrap()
                .filter_map(Result::ok)
                .collect();
            assert!(names.iter().any(|n| n == column), "missing column {column}");
        }
        let pool: DbPool = Arc::new(crate::db::Db::from_connection(conn));
        let id = sessions_repo::get_or_create_session(&pool, "mtg-mig", None, None).unwrap();
        let fresh = sessions_repo::get_session_by_meeting_key(&pool, "mtg-mig")
            .unwrap()
            .expect("session by key");
        assert!(fresh.flow_id.is_none());
        assert_eq!(
            SessionPipeline::from_row(&fresh).flow_id,
            MEETING_BOT_FLOW_ID,
            "NULL flow resolves to the factory flow"
        );
        sessions_repo::update_session_pipeline(&pool, id, "s", "l", "t", "flow-x").unwrap();
        let row = sessions_repo::get_session_by_meeting_key(&pool, "mtg-mig")
            .unwrap()
            .unwrap();
        assert_eq!(row.id, id);
        assert_eq!(row.stt_alias.as_deref(), Some("s"));
        assert_eq!(row.llm_alias.as_deref(), Some("l"));
        assert_eq!(row.tts_alias.as_deref(), Some("t"));
        assert_eq!(row.flow_id.as_deref(), Some("flow-x"));
        assert!(sessions_repo::get_session_by_meeting_key(&pool, "nope")
            .unwrap()
            .is_none());
    }

    #[test]
    fn pipeline_meta_follows_envelope_contract() {
        let mut env = FlowEnvelope::empty();
        let p = SessionPipeline {
            stt_alias: "s".into(),
            llm_alias: "l".into(),
            tts_alias: "t".into(),
            flow_id: "f".into(),
        };
        apply_pipeline_meta(&mut env, &p, "mtg", Some("pl"), true);
        assert!(env.wants_output_audio());
        assert_eq!(env.meta["model"], "l");
        assert_eq!(env.meta["stt_model"], "s");
        assert_eq!(env.meta["tts_model"], "t");
        assert_eq!(env.meta["language"], "pl");
        assert_eq!(env.meta["format"], "pcm");
        apply_pipeline_meta(&mut env, &p, "mtg", None, false);
        assert!(!env.wants_output_audio());
    }
}
