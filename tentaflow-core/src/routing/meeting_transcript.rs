// =============================================================================
// Plik: routing/meeting_transcript.rs
// Opis: Subskrypcja streamingu transkrypcji z kontenera meeting bot.
//       Otwiera stream QUIC z request stream=true, odbiera chunki transkrypcji
//       (length-prefixed rkyv ModelStreamChunk), publikuje jako eventy
//       "meeting.transcript" do addon EventBus.
// =============================================================================

use std::sync::Arc;

use anyhow::Context;
use serde_json::json;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::addon::event_bus::{Event, EventBus};
use crate::net::quic::QuicClient;

/// Maksymalny rozmiar pojedynczego chunka transkrypcji (1 MB)
const MAX_CHUNK_SIZE: usize = 1_024 * 1_024;

/// Uruchamia background task subskrybujacy streaming transkrypcji z kontenera meeting bot.
///
/// Punkt integracji: wywolaj po nawiazaniu polaczenia QUIC z kontenerem teams-bot,
/// np. w `ServiceManager` po `set_connected()` dla serwisu typu MeetingBot.
///
/// Parametry:
/// - `quic_client` — polaczony klient QUIC do kontenera meeting bot
/// - `event_bus` — referencja do globalnego EventBus addonow
/// - `shutdown_rx` — sygnal shutdown (watch channel)
///
/// Zwraca JoinHandle taska — mozna go uzyc do anulowania subskrypcji.
pub fn spawn_transcript_subscriber(
    quic_client: Arc<QuicClient>,
    event_bus: Arc<EventBus>,
    shutdown_rx: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = subscribe_meeting_transcripts(quic_client, event_bus, shutdown_rx).await {
            error!("Subskrypcja transkrypcji zakonczyla sie bledem: {}", e);
        }
    })
}

/// Subskrybuje streaming transkrypcji z kontenera meeting bot.
///
/// Otwiera bidirektionalny stream QUIC, wysyla ModelRequest z stream=true,
/// a nastepnie w petli odbiera chunki ModelStreamChunk (4-bajtowy length prefix
/// big-endian + rkyv payload). Kazdy chunk TextDelta o formacie `[speaker]: text\n`
/// jest parsowany i publikowany jako event "meeting.transcript" do EventBus.
async fn subscribe_meeting_transcripts(
    quic_client: Arc<QuicClient>,
    event_bus: Arc<EventBus>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    info!("Rozpoczynam subskrypcje transkrypcji z meeting bot");

    // Pobierz aktywne polaczenie iroh (z auto-reconnect).
    let conn = quic_client
        .iroh_connection()
        .await
        .context("Brak aktywnego polaczenia iroh do meeting bot")?;

    // Otworz bidirektionalny stream
    let (mut send, mut recv) = conn
        .open_bi()
        .await
        .context("Nie udalo sie otworzyc streamu QUIC do meeting bot")?;

    // Zbuduj ModelRequest z stream=true
    let request = tentaflow_protocol::ModelRequest {
        request_id: format!("transcript-stream-{}", uuid::Uuid::new_v4()),
        payload: tentaflow_protocol::ModelPayload::Completion(
            tentaflow_protocol::CompletionPayload {
                model: "meeting-bot".to_string(),
                prompt: None,
                messages: vec![tentaflow_protocol::Message {
                    role: "system".to_string(),
                    content: "stream-transcripts".to_string(),
                }],
                temperature: None,
                max_tokens: None,
                top_p: None,
                stop: None,
                presence_penalty: None,
                frequency_penalty: None,
                tts_options: None,
                memory_options: None,
                audio_input: None,
                prefix_cache_id: None,
                prefix_text: None,
            },
        ),
        stream: true,
        metadata: None,
        session_id: None,
    };

    // Serializuj i wyslij request
    let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request)
        .map_err(|e| anyhow::anyhow!("Blad serializacji ModelRequest: {}", e))?;

    send.write_all(&request_bytes).await?;
    send.finish()?;

    info!("Wyslano ModelRequest stream=true do meeting bot, oczekuje na chunki");

    // Petla odbioru chunkow (length-prefixed rkyv)
    loop {
        // Sprawdz sygnal shutdown przed kazdym odczytem
        tokio::select! {
            biased;

            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("Subskrypcja transkrypcji: sygnal shutdown");
                    return Ok(());
                }
            }

            result = read_stream_chunk(&mut recv) => {
                match result {
                    Ok(Some(chunk)) => {
                        handle_transcript_chunk(&chunk, &event_bus);
                    }
                    Ok(None) => {
                        info!("Stream transkrypcji zakonczony (sidecar zamknal stream)");
                        return Ok(());
                    }
                    Err(e) => {
                        return Err(e.context("Blad odczytu chunka transkrypcji"));
                    }
                }
            }
        }
    }
}

/// Odczytuje pojedynczy chunk ze streamu QUIC (4-bajtowy length prefix + rkyv payload).
/// Zwraca None jesli stream zostal zamkniety.
async fn read_stream_chunk(
    recv: &mut iroh::endpoint::RecvStream,
) -> anyhow::Result<Option<tentaflow_protocol::ModelStreamChunk>> {
    use iroh::endpoint::ReadExactError;

    // Odczytaj 4 bajty dlugosci (big-endian)
    let mut len_buf = [0u8; 4];
    match recv.read_exact(&mut len_buf).await {
        Ok(()) => {}
        Err(ReadExactError::FinishedEarly(_)) => return Ok(None),
        Err(e) => return Err(anyhow::anyhow!("Blad odczytu dlugosci chunka: {}", e)),
    }

    let chunk_len = u32::from_be_bytes(len_buf) as usize;
    if chunk_len > MAX_CHUNK_SIZE {
        anyhow::bail!(
            "Chunk transkrypcji za duzy: {} bajtow (limit {})",
            chunk_len,
            MAX_CHUNK_SIZE
        );
    }

    // Odczytaj payload
    let mut chunk_buf = vec![0u8; chunk_len];
    match recv.read_exact(&mut chunk_buf).await {
        Ok(()) => {}
        Err(ReadExactError::FinishedEarly(_)) => return Ok(None),
        Err(e) => return Err(anyhow::anyhow!("Blad odczytu payloadu chunka: {}", e)),
    }

    // Deserializuj ModelStreamChunk (rkyv zero-copy)
    let archived =
        rkyv::access::<tentaflow_protocol::ArchivedModelStreamChunk, rkyv::rancor::Error>(
            &chunk_buf,
        )
        .map_err(|e| anyhow::anyhow!("Blad walidacji rkyv ModelStreamChunk: {}", e))?;

    let chunk =
        rkyv::deserialize::<tentaflow_protocol::ModelStreamChunk, rkyv::rancor::Error>(archived)
            .map_err(|e| anyhow::anyhow!("Blad deserializacji rkyv ModelStreamChunk: {}", e))?;

    Ok(Some(chunk))
}

/// Przetwarza chunk transkrypcji i publikuje do EventBus.
///
/// Format TextDelta z sidecar: `[speaker]: text\n`
/// Parsuje speaker i text, publikuje jako event "meeting.transcript".
fn handle_transcript_chunk(chunk: &tentaflow_protocol::ModelStreamChunk, event_bus: &EventBus) {
    match &chunk.chunk {
        tentaflow_protocol::StreamChunkType::TextDelta(delta) => {
            let (speaker, text) = parse_transcript_delta(delta);

            debug!(
                request_id = %chunk.request_id,
                speaker = %speaker,
                "Transkrypcja: {} znakow",
                text.len()
            );

            let now = chrono::Utc::now();

            event_bus.publish(Event {
                event_type: "meeting.transcript".to_string(),
                source_addon: None,
                source_user: None,
                payload: json!({
                    "speaker": speaker,
                    "text": text,
                    "timestamp_ms": now.timestamp_millis(),
                    "request_id": chunk.request_id,
                }),
                timestamp: now,
            });
        }
        tentaflow_protocol::StreamChunkType::Done { .. } => {
            info!(
                request_id = %chunk.request_id,
                "Stream transkrypcji: otrzymano Done"
            );
        }
        tentaflow_protocol::StreamChunkType::Error(err) => {
            warn!(
                request_id = %chunk.request_id,
                "Stream transkrypcji: blad z sidecar: {}",
                err.message
            );
        }
        other => {
            debug!(
                request_id = %chunk.request_id,
                "Stream transkrypcji: pominieto chunk typu {:?}",
                std::mem::discriminant(other)
            );
        }
    }
}

/// Parsuje format `[speaker]: text\n` z TextDelta.
/// Jesli format nie pasuje, zwraca ("unknown", caly tekst).
fn parse_transcript_delta(delta: &str) -> (&str, &str) {
    let trimmed = delta.trim_end_matches('\n');

    if let Some(rest) = trimmed.strip_prefix('[') {
        if let Some(bracket_end) = rest.find(']') {
            let speaker = &rest[..bracket_end];
            let after_bracket = &rest[bracket_end + 1..];
            let text = after_bracket.strip_prefix(": ").unwrap_or(after_bracket);
            return (speaker, text);
        }
    }

    ("unknown", trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_transcript_delta_standard() {
        let (speaker, text) = parse_transcript_delta("[Jan Kowalski]: Dzien dobry\n");
        assert_eq!(speaker, "Jan Kowalski");
        assert_eq!(text, "Dzien dobry");
    }

    #[test]
    fn test_parse_transcript_delta_no_newline() {
        let (speaker, text) = parse_transcript_delta("[Anna]: Czesc");
        assert_eq!(speaker, "Anna");
        assert_eq!(text, "Czesc");
    }

    #[test]
    fn test_parse_transcript_delta_empty_text() {
        let (speaker, text) = parse_transcript_delta("[Bot]: ");
        assert_eq!(speaker, "Bot");
        assert_eq!(text, "");
    }

    #[test]
    fn test_parse_transcript_delta_malformed() {
        let (speaker, text) = parse_transcript_delta("nieprawidlowy format");
        assert_eq!(speaker, "unknown");
        assert_eq!(text, "nieprawidlowy format");
    }

    #[test]
    fn test_parse_transcript_delta_brackets_in_text() {
        let (speaker, text) = parse_transcript_delta("[Ola]: tekst [z nawiasami]\n");
        assert_eq!(speaker, "Ola");
        assert_eq!(text, "tekst [z nawiasami]");
    }

    #[test]
    fn test_handle_transcript_chunk_text_delta() {
        let bus = EventBus::new();

        let chunk = tentaflow_protocol::ModelStreamChunk {
            request_id: "test-123".to_string(),
            chunk: tentaflow_protocol::StreamChunkType::TextDelta(
                "[Marek]: Testowa wiadomosc\n".to_string(),
            ),
        };

        handle_transcript_chunk(&chunk, &bus);

        let events = bus.recent_events(1);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "meeting.transcript");
        assert_eq!(events[0].payload["speaker"], "Marek");
        assert_eq!(events[0].payload["text"], "Testowa wiadomosc");
        assert!(events[0].payload["timestamp_ms"].is_number());
        assert_eq!(events[0].payload["request_id"], "test-123");
        assert!(events[0].source_addon.is_none());
    }

    #[test]
    fn test_handle_transcript_chunk_done() {
        let bus = EventBus::new();

        let chunk = tentaflow_protocol::ModelStreamChunk {
            request_id: "test-456".to_string(),
            chunk: tentaflow_protocol::StreamChunkType::Done {
                final_metrics: None,
            },
        };

        // Nie powinno publikowac eventu
        handle_transcript_chunk(&chunk, &bus);
        let events = bus.recent_events(1);
        assert_eq!(events.len(), 0);
    }
}
