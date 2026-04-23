// =============================================================================
// Plik: routing/transcript_store.rs
// Opis: Live ring buffer (do widoku w GUI) + trwala persystencja w SQLite
//       (tabele meeting_sessions / meeting_transcripts). Zapis do bazy
//       jest synchroniczny i nieblokujacy ring-buffera — wszystkie wpisy
//       trafiaja do DB nawet po restarcie procesu.
// =============================================================================

use parking_lot::RwLock;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::OnceLock;

/// Ring buffer wylacznie dla szybkiego live-widoku w GUI. Trwale dane sa w DB.
const MAX_LIVE_TRANSCRIPTS: usize = 5000;

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct TranscriptEntry {
    /// Unix timestamp w milisekundach (UTC)
    pub timestamp_ms: u64,
    /// Imie (enrolled) albo "SPEAKER_XX"
    pub speaker: String,
    /// DB id profilu voice_profiles, jesli to enrolled match
    pub profile_id: Option<i64>,
    /// Score z matchingu (0.0-1.0)
    pub confidence: Option<f32>,
    /// Czy to enrolled profile (true) czy temp speaker (false)
    pub is_enrolled: bool,
    /// Meeting_id (klucz logiczny rozmowy, np. UUID przekazany z bota)
    pub meeting_id: Option<String>,
    /// Transkrybowany tekst
    pub text: String,
    /// Nazwa modelu STT
    pub model: String,
}

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
    STORE.get_or_init(|| RwLock::new(VecDeque::with_capacity(MAX_LIVE_TRANSCRIPTS)))
}

/// Aktywna sesja — meeting_key + DB session_id. Gdy meeting_key sie zmieni,
/// otwierana jest nowa sesja w bazie (lub odzyskiwana po istniejacym kluczu).
struct ActiveSession {
    meeting_key: String,
    session_id: i64,
}

static ACTIVE_SESSION: OnceLock<parking_lot::Mutex<Option<ActiveSession>>> = OnceLock::new();

fn active_session() -> &'static parking_lot::Mutex<Option<ActiveSession>> {
    ACTIVE_SESSION.get_or_init(|| parking_lot::Mutex::new(None))
}

/// Klucz sesji do tabeli meeting_sessions — preferujemy meeting_id z bota,
/// w przypadku braku uzywamy "standalone".
fn session_key_for(entry: &TranscriptEntry) -> String {
    entry
        .meeting_id
        .clone()
        .unwrap_or_else(|| "standalone".to_string())
}

/// Pobiera/tworzy session_id w DB dla danego meeting_key i zapisuje wpis.
/// Bledy DB sa logowane (nie blokujemy live-widoku).
fn persist_to_db(entry: &TranscriptEntry) {
    let pool = match crate::db::global_pool() {
        Some(p) => p,
        None => return,
    };
    let key = session_key_for(entry);

    let mut guard = active_session().lock();
    let need_new = match guard.as_ref() {
        Some(s) => s.meeting_key != key,
        None => true,
    };

    let session_id = if need_new {
        match crate::db::repository::transcripts::get_or_create_session(&pool, &key, None, None) {
            Ok(id) => {
                *guard = Some(ActiveSession {
                    meeting_key: key.clone(),
                    session_id: id,
                });
                id
            }
            Err(e) => {
                tracing::warn!("transcripts: nie udalo sie utworzyc sesji '{}': {}", key, e);
                return;
            }
        }
    } else {
        guard.as_ref().map(|s| s.session_id).unwrap_or(0)
    };

    if let Err(e) = crate::db::repository::transcripts::insert_transcript(&pool, session_id, entry)
    {
        tracing::warn!("transcripts: nie udalo sie zapisac wpisu: {}", e);
    }
}

/// Dodaje wpis — ring buffer (live) + trwale do DB.
pub fn push_entry(entry: TranscriptEntry) {
    {
        let mut guard = store().write();
        if guard.len() >= MAX_LIVE_TRANSCRIPTS {
            guard.pop_front();
        }
        guard.push_back(entry.clone());
    }
    persist_to_db(&entry);
}

pub fn push(builder: TranscriptBuilder) {
    push_entry(builder.build());
}

#[allow(dead_code)]
pub fn clear() {
    store().write().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex as StdMutex, MutexGuard};

    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    fn isolate() -> MutexGuard<'static, ()> {
        let g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        *active_session().lock() = None;
        clear();
        g
    }

    /// Lokalny helper testowy — snapshot ring-buffera (od najnowszej).
    fn snapshot() -> Vec<TranscriptEntry> {
        store().read().iter().rev().cloned().collect()
    }

    #[test]
    fn builder_defaults() {
        let _g = isolate();
        push(TranscriptBuilder::new("Hello", "whisper-1").speaker("SPEAKER_00"));
        let entries = snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].speaker, "SPEAKER_00");
        assert_eq!(entries[0].text, "Hello");
        assert!(!entries[0].is_enrolled);
    }

    #[test]
    fn builder_with_enrolled_profile() {
        let _g = isolate();
        push(
            TranscriptBuilder::new("Witam", "whisper-1")
                .speaker("Jan Kowalski")
                .profile_id(42)
                .confidence(0.91)
                .meeting_id("meet-abc"),
        );
        let entries = snapshot();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].profile_id, Some(42));
        assert!(entries[0].is_enrolled);
    }

    #[test]
    fn ring_rotates_at_capacity() {
        let _g = isolate();
        for i in 0..(MAX_LIVE_TRANSCRIPTS + 50) {
            push(TranscriptBuilder::new(format!("msg-{}", i), "w"));
        }
        let entries = snapshot();
        assert_eq!(entries.len(), MAX_LIVE_TRANSCRIPTS);
    }
}
