// =============================================================================
// Plik: routing/transcript_store.rs
// Opis: Globalny ring buffer ostatnich transkrypcji z meeting-bota. Wpisy sa
//       generowane przez reverse_request po odebraniu audio/transkrypcji z
//       STT + wyniku speaker identification. Czytane przez endpoint
//       /api/meeting-bot/transcripts i renderowane w GUI Bot Status.
//
//       Kazdy wpis ma:
//         - speaker label (enrolled name albo SPEAKER_XX)
//         - profile_id (Some jesli to enrolled profile)
//         - confidence (score z matchingu, 0.0-1.0)
//         - is_enrolled (bool — czy to enrolled profile czy temp speaker)
//         - meeting_id (do grupowania po meetingach w GUI)
// =============================================================================

use parking_lot::RwLock;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::OnceLock;

const MAX_TRANSCRIPTS: usize = 200;

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptEntry {
    /// Unix timestamp w milisekundach (UTC)
    pub timestamp_ms: u64,
    /// Imie (enrolled) albo "SPEAKER_XX"
    pub speaker: String,
    /// DB id profilu voice_profiles, jesli to enrolled match
    pub profile_id: Option<i64>,
    /// Score z matchingu (0.0-1.0). Dla enrolled to confidence vs profile,
    /// dla temp speakera to similarity wew online clusteringu.
    pub confidence: Option<f32>,
    /// Czy to enrolled profile (true) czy temp speaker (false)
    pub is_enrolled: bool,
    /// Meeting_id z ktorego pochodzi transcript (None dla standalone STT)
    pub meeting_id: Option<String>,
    /// Transkrybowany tekst
    pub text: String,
    /// Nazwa modelu STT
    pub model: String,
}

/// Budowniczy wpisu — caller ustawia poszczegolne pola, timestamp dopisywany auto
#[derive(Debug, Clone, Default)]
pub struct TranscriptBuilder {
    pub speaker: String,
    pub profile_id: Option<i64>,
    pub confidence: Option<f32>,
    pub is_enrolled: bool,
    pub meeting_id: Option<String>,
    pub text: String,
    pub model: String,
}

impl TranscriptBuilder {
    pub fn new(text: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            speaker: "Nieznany".to_string(),
            profile_id: None,
            confidence: None,
            is_enrolled: false,
            meeting_id: None,
            text: text.into(),
            model: model.into(),
        }
    }

    pub fn speaker(mut self, label: impl Into<String>) -> Self {
        self.speaker = label.into();
        self
    }

    pub fn profile_id(mut self, id: i64) -> Self {
        self.profile_id = Some(id);
        self.is_enrolled = true;
        self
    }

    pub fn confidence(mut self, c: f32) -> Self {
        self.confidence = Some(c);
        self
    }

    pub fn meeting_id(mut self, mid: impl Into<String>) -> Self {
        self.meeting_id = Some(mid.into());
        self
    }

    pub fn build(self) -> TranscriptEntry {
        TranscriptEntry {
            timestamp_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            speaker: self.speaker,
            profile_id: self.profile_id,
            confidence: self.confidence,
            is_enrolled: self.is_enrolled,
            meeting_id: self.meeting_id,
            text: self.text,
            model: self.model,
        }
    }
}

static STORE: OnceLock<RwLock<VecDeque<TranscriptEntry>>> = OnceLock::new();

fn store() -> &'static RwLock<VecDeque<TranscriptEntry>> {
    STORE.get_or_init(|| RwLock::new(VecDeque::with_capacity(MAX_TRANSCRIPTS)))
}

/// Dodaje nowy wpis — przyjmuje w pelni zbudowany TranscriptEntry.
pub fn push_entry(entry: TranscriptEntry) {
    let mut guard = store().write();
    if guard.len() >= MAX_TRANSCRIPTS {
        guard.pop_front();
    }
    guard.push_back(entry);
}

/// Shortcut — buduje i zapisuje w jednym kroku
pub fn push(builder: TranscriptBuilder) {
    push_entry(builder.build());
}

/// Zwraca ostatnie `limit` transkrypcji (od najnowszej)
pub fn list(limit: usize) -> Vec<TranscriptEntry> {
    let guard = store().read();
    guard.iter().rev().take(limit).cloned().collect()
}

/// Zwraca transkrypcje dla konkretnego meetingu
pub fn list_for_meeting(meeting_id: &str, limit: usize) -> Vec<TranscriptEntry> {
    let guard = store().read();
    guard
        .iter()
        .rev()
        .filter(|e| e.meeting_id.as_deref() == Some(meeting_id))
        .take(limit)
        .cloned()
        .collect()
}

/// Czysci wszystkie transkrypcje
#[allow(dead_code)]
pub fn clear() {
    store().write().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults() {
        clear();
        push(TranscriptBuilder::new("Hello", "whisper-1").speaker("SPEAKER_00"));
        let entries = list(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].speaker, "SPEAKER_00");
        assert_eq!(entries[0].text, "Hello");
        assert!(!entries[0].is_enrolled);
        assert_eq!(entries[0].profile_id, None);
    }

    #[test]
    fn builder_with_enrolled_profile() {
        clear();
        push(
            TranscriptBuilder::new("Witam", "whisper-1")
                .speaker("Jan Kowalski")
                .profile_id(42)
                .confidence(0.91)
                .meeting_id("meet-abc"),
        );
        let entries = list(10);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].speaker, "Jan Kowalski");
        assert_eq!(entries[0].profile_id, Some(42));
        assert!(entries[0].is_enrolled);
        assert!((entries[0].confidence.unwrap() - 0.91).abs() < 1e-5);
        assert_eq!(entries[0].meeting_id.as_deref(), Some("meet-abc"));
    }

    #[test]
    fn list_for_meeting_filters() {
        clear();
        push(TranscriptBuilder::new("A", "w").meeting_id("meet-1"));
        push(TranscriptBuilder::new("B", "w").meeting_id("meet-2"));
        push(TranscriptBuilder::new("C", "w").meeting_id("meet-1"));
        let m1 = list_for_meeting("meet-1", 10);
        assert_eq!(m1.len(), 2);
        assert_eq!(m1[0].text, "C"); // newest first
        assert_eq!(m1[1].text, "A");
    }

    #[test]
    fn push_rotates_at_capacity() {
        clear();
        for i in 0..(MAX_TRANSCRIPTS + 50) {
            push(TranscriptBuilder::new(format!("msg-{}", i), "w"));
        }
        let entries = list(MAX_TRANSCRIPTS + 100);
        assert_eq!(entries.len(), MAX_TRANSCRIPTS);
        assert_eq!(entries[0].text, format!("msg-{}", MAX_TRANSCRIPTS + 49));
    }
}
