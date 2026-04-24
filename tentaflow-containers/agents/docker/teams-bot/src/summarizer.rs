// =============================================================================
// Plik: summarizer.rs
// Opis: Rolling buffer transkrypcji spotkania + petla timerowa generujaca
//       podsumowanie przez RouterClient::chat_completion i wysylajaca
//       MeetingEvent (SummaryUpdate + ActionItemsUpdate) do routera.
// =============================================================================

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::Deserialize;
use tentaflow_protocol::{MeetingActionItemData, MeetingEventPayload};
use tokio::sync::{watch, Mutex};
use tokio::time::{interval, Duration};
use tracing::{debug, info, warn};

use crate::quic_server::RouterClient;

/// Pojedynczy wpis transkrypcji w bufferze.
#[derive(Debug, Clone)]
pub struct TranscriptEntry {
    /// Unix epoch ms — moment w ktorym STT zwrocilo tekst.
    pub timestamp_ms: i64,
    /// Etykieta mowcy ("Jan Kowalski" albo "SPEAKER_00" jesli brak diarization).
    pub speaker_name: String,
    /// Tekst wypowiedzi.
    pub text: String,
}

/// Rolling buffer wpisow transkrypcji. Przycina wpisy starsze niz
/// `max_duration_ms` przy kazdym push — LLM zawsze widzi ostatnie N minut.
pub struct TranscriptBuffer {
    entries: Vec<TranscriptEntry>,
    max_duration_ms: i64,
}

impl TranscriptBuffer {
    pub fn new(max_duration_secs: i64) -> Self {
        Self {
            entries: Vec::new(),
            max_duration_ms: max_duration_secs.saturating_mul(1000),
        }
    }

    pub fn push(&mut self, entry: TranscriptEntry) {
        self.entries.push(entry);
        self.prune();
    }

    /// Usuwa wpisy starsze niz `max_duration_ms` wzgledem najnowszego wpisu.
    /// Bazujemy na timestampach wpisow (a nie wall-clock), zeby testy byly
    /// deterministyczne i zeby pauzy w spotkaniu nie zrzucaly bufferu.
    fn prune(&mut self) {
        let Some(newest_ts) = self.entries.last().map(|e| e.timestamp_ms) else {
            return;
        };
        let cutoff = newest_ts - self.max_duration_ms;
        self.entries.retain(|e| e.timestamp_ms >= cutoff);
    }

    /// Formatuje buffer do promptu: `[speaker] text` per linia, w kolejnosci
    /// chronologicznej. Zgodnie ze schematem w seed prompta.
    pub fn format_for_llm(&self) -> String {
        let mut out = String::new();
        for e in &self.entries {
            out.push('[');
            out.push_str(&e.speaker_name);
            out.push_str("] ");
            out.push_str(&e.text);
            out.push('\n');
        }
        out
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Czysci wszystkie wpisy — wolane przy zmianie sesji spotkania, zeby
    /// transkrypty starego meetingu nie trafily do promptu nowego.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// JSON ktory LLM ma zwrocic zgodnie z promptem `transcription_summarization`.
/// Klucze sa w angielskim snake_case niezaleznie od jezyka instrukcji.
#[derive(Debug, Deserialize)]
struct SummaryJson {
    decisions: String,
    action_items: Vec<SummaryActionItemJson>,
    summary_text: String,
}

#[derive(Debug, Deserialize)]
struct SummaryActionItemJson {
    owner: String,
    task: String,
    #[serde(default)]
    deadline: Option<String>,
}

/// Uruchamiany jako `tokio::spawn` na start sesji spotkania. Konczy sie gdy
/// `shutdown_rx` zmieni stan na `true`.
#[allow(clippy::too_many_arguments)]
pub async fn run_summarizer_loop(
    buffer: Arc<Mutex<TranscriptBuffer>>,
    router: Arc<tokio::sync::Mutex<Option<Arc<RouterClient>>>>,
    interval_secs: u64,
    min_entries_threshold: usize,
    meeting_key: String,
    summarization_alias: String,
    prompt_content: String,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let mut ticker = interval(Duration::from_secs(interval_secs.max(1)));
    // Pierwszy tick leci natychmiast — skip zeby czekac pelny interval przed pierwszym runem.
    ticker.tick().await;

    info!(
        meeting_key = %meeting_key,
        interval_secs,
        min_entries_threshold,
        alias = %summarization_alias,
        "Summarizer loop uruchomiony"
    );

    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("Summarizer loop: shutdown");
                    break;
                }
            }
        }

        // Snapshot bufferu pod lockiem — lock trzymany minimalnie, LLM call poza.
        let transcript = {
            let buf = buffer.lock().await;
            if buf.len() < min_entries_threshold {
                debug!(entries = buf.len(), "Za malo wpisow — pomijam tick");
                continue;
            }
            buf.format_for_llm()
        };

        // Pobierz aktualny RouterClient — router moze byc miedzy rekonektami.
        let client = {
            let guard = router.lock().await;
            guard.as_ref().cloned()
        };
        let Some(client) = client else {
            warn!("Summarizer: router client niedostepny — pomijam tick");
            continue;
        };

        let messages = vec![
            ("system".to_string(), prompt_content.clone()),
            ("user".to_string(), transcript),
        ];

        let llm_result = match tokio::time::timeout(
            Duration::from_secs(60),
            client.chat_completion(&summarization_alias, messages),
        )
        .await
        {
            Ok(r) => r,
            Err(_) => {
                warn!("Summarizer: chat_completion timeout po 60s");
                continue;
            }
        };

        let result = match llm_result {
            Ok(r) => r,
            Err(e) => {
                warn!("Summarizer: chat_completion failed: {}", e);
                continue;
            }
        };

        let parsed = match parse_summary_json(&result.content) {
            Some(p) => p,
            None => {
                let preview: String = result.content.chars().take(200).collect();
                warn!(
                    model = %result.resolved_model,
                    preview = %preview,
                    "Summarizer: LLM zwrocil nie-JSON albo niepoprawny schemat — skip"
                );
                continue;
            }
        };

        let timestamp_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        if let Err(e) = client
            .send_meeting_event(
                &meeting_key,
                timestamp_ms,
                MeetingEventPayload::SummaryUpdate {
                    decisions_text: parsed.decisions.clone(),
                    summary_text: parsed.summary_text.clone(),
                    model: result.resolved_model.clone(),
                },
            )
            .await
        {
            warn!("Summarizer: send_meeting_event SummaryUpdate failed: {}", e);
        }

        if !parsed.action_items.is_empty() {
            let items: Vec<MeetingActionItemData> = parsed
                .action_items
                .iter()
                .map(|a| MeetingActionItemData {
                    owner: a.owner.clone(),
                    task: a.task.clone(),
                    deadline: a.deadline.clone(),
                })
                .collect();
            let n = items.len();
            if let Err(e) = client
                .send_meeting_event(
                    &meeting_key,
                    timestamp_ms,
                    MeetingEventPayload::ActionItemsUpdate { items },
                )
                .await
            {
                warn!(
                    "Summarizer: send_meeting_event ActionItemsUpdate failed: {}",
                    e
                );
            } else {
                info!("Summarizer: wyslano {} action items", n);
            }
        }

        info!(
            model = %result.resolved_model,
            decisions_len = parsed.decisions.len(),
            summary_len = parsed.summary_text.len(),
            action_items = parsed.action_items.len(),
            "Summarizer: wygenerowano podsumowanie"
        );
    }

    Ok(())
}

/// Parsuje odpowiedz LLM. Probuje najpierw czysty JSON, a jesli sie nie udalo —
/// szuka pierwszego `{` i ostatniego `}` (modele czasem opakowuja w markdown
/// mimo instrukcji "return only JSON").
fn parse_summary_json(raw: &str) -> Option<SummaryJson> {
    if let Ok(v) = serde_json::from_str::<SummaryJson>(raw.trim()) {
        return Some(v);
    }
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str::<SummaryJson>(&raw[start..=end]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ts_ms: i64, speaker: &str, text: &str) -> TranscriptEntry {
        TranscriptEntry {
            timestamp_ms: ts_ms,
            speaker_name: speaker.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn buffer_prunes_entries_older_than_window() {
        // Window 60s — najnowszy wpis 100_000 ms, wiec cutoff = 40_000 ms.
        let mut buf = TranscriptBuffer::new(60);
        buf.push(entry(10_000, "A", "stary"));
        buf.push(entry(30_000, "A", "tez stary"));
        buf.push(entry(50_000, "B", "niedawno"));
        buf.push(entry(100_000, "C", "teraz"));
        assert_eq!(buf.len(), 2, "tylko wpisy >= 40_000 zostaja");
        assert_eq!(buf.entries[0].text, "niedawno");
        assert_eq!(buf.entries[1].text, "teraz");
    }

    #[test]
    fn buffer_format_preserves_order_and_labels() {
        let mut buf = TranscriptBuffer::new(3600);
        buf.push(entry(1000, "Jan", "Witajcie."));
        buf.push(entry(2000, "SPEAKER_01", "Czesc."));
        let out = buf.format_for_llm();
        assert_eq!(out, "[Jan] Witajcie.\n[SPEAKER_01] Czesc.\n");
    }

    #[test]
    fn buffer_empty_format_is_empty_string() {
        let buf = TranscriptBuffer::new(60);
        assert_eq!(buf.len(), 0);
        assert_eq!(buf.format_for_llm(), "");
    }

    #[test]
    fn buffer_clear_removes_entries() {
        let mut buf = TranscriptBuffer::new(3600);
        buf.push(entry(1000, "A", "test"));
        buf.push(entry(2000, "B", "test2"));
        assert_eq!(buf.len(), 2);
        buf.clear();
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn parse_summary_clean_json() {
        let raw = r#"{
            "decisions": "Zdecydowano X.",
            "action_items": [
                {"owner": "Ania", "task": "Zrobic Y", "deadline": "piatek"}
            ],
            "summary_text": "Podsumowanie."
        }"#;
        let p = parse_summary_json(raw).expect("parse");
        assert_eq!(p.decisions, "Zdecydowano X.");
        assert_eq!(p.action_items.len(), 1);
        assert_eq!(p.action_items[0].owner, "Ania");
        assert_eq!(p.action_items[0].deadline.as_deref(), Some("piatek"));
    }

    #[test]
    fn parse_summary_json_wrapped_in_markdown() {
        // Model czasem opakowuje w ```json ... ``` mimo instrukcji. Parser
        // musi znalezc pierwszy '{' i ostatni '}'.
        let raw = "```json\n{\"decisions\":\"D\",\"action_items\":[],\"summary_text\":\"S\"}\n```";
        let p = parse_summary_json(raw).expect("parse");
        assert_eq!(p.decisions, "D");
        assert!(p.action_items.is_empty());
    }

    #[test]
    fn parse_summary_rejects_garbage() {
        assert!(parse_summary_json("nope").is_none());
        assert!(parse_summary_json("{\"wrong\": true}").is_none());
    }

    #[test]
    fn parse_summary_allows_missing_deadline() {
        let raw = r#"{"decisions":"D","action_items":[{"owner":"X","task":"T"}],"summary_text":"S"}"#;
        let p = parse_summary_json(raw).expect("parse");
        assert_eq!(p.action_items[0].deadline, None);
    }

}
