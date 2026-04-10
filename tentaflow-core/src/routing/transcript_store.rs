// =============================================================================
// Plik: routing/transcript_store.rs
// Opis: Globalny ring buffer ostatnich transkrypcji z meeting-bot, wypelniany
//       przez reverse_request gdy bot wysyla audio do STT. Czytany przez
//       endpoint /api/meeting-bot/transcripts i wyswietlany w GUI Bot Status.
// =============================================================================

use std::collections::VecDeque;
use std::sync::OnceLock;
use parking_lot::RwLock;
use serde::Serialize;

/// Maksymalna liczba przechowywanych transkrypcji
const MAX_TRANSCRIPTS: usize = 200;

/// Pojedynczy wpis transkrypcji
#[derive(Debug, Clone, Serialize)]
pub struct TranscriptEntry {
    /// Unix timestamp w milisekundach
    pub timestamp_ms: u64,
    /// Nazwa mowcy (lub "Nieznany")
    pub speaker: String,
    /// Transkrybowany tekst
    pub text: String,
    /// Nazwa modelu STT (np. "whisper-stt-native")
    pub model: String,
}

/// Globalny store (lazy init)
static STORE: OnceLock<RwLock<VecDeque<TranscriptEntry>>> = OnceLock::new();

fn store() -> &'static RwLock<VecDeque<TranscriptEntry>> {
    STORE.get_or_init(|| RwLock::new(VecDeque::with_capacity(MAX_TRANSCRIPTS)))
}

/// Dodaje nowa transkrypcje do store (auto-rotate przy przekroczeniu MAX_TRANSCRIPTS)
pub fn push(speaker: impl Into<String>, text: impl Into<String>, model: impl Into<String>) {
    let entry = TranscriptEntry {
        timestamp_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
        speaker: speaker.into(),
        text: text.into(),
        model: model.into(),
    };
    let mut guard = store().write();
    if guard.len() >= MAX_TRANSCRIPTS {
        guard.pop_front();
    }
    guard.push_back(entry);
}

/// Zwraca ostatnie `limit` transkrypcji (od najnowszej)
pub fn list(limit: usize) -> Vec<TranscriptEntry> {
    let guard = store().read();
    guard.iter().rev().take(limit).cloned().collect()
}

/// Czysci wszystkie transkrypcje (np. przy LeaveMeeting)
#[allow(dead_code)]
pub fn clear() {
    store().write().clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_list_returns_newest_first() {
        clear();
        push("Adam", "Pierwsza", "whisper");
        push("Ewa", "Druga", "whisper");
        let entries = list(10);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].text, "Druga");
        assert_eq!(entries[1].text, "Pierwsza");
    }

    #[test]
    fn push_rotates_at_capacity() {
        clear();
        for i in 0..(MAX_TRANSCRIPTS + 50) {
            push("X", format!("msg-{}", i), "whisper");
        }
        let entries = list(MAX_TRANSCRIPTS + 100);
        assert_eq!(entries.len(), MAX_TRANSCRIPTS);
        // Najnowsza powinna byc ostatnia
        assert_eq!(entries[0].text, format!("msg-{}", MAX_TRANSCRIPTS + 49));
    }
}
