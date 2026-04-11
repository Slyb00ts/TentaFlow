// =============================================================================
// Plik: diarization/tracker.rs
// Opis: Per-meeting speaker tracker z persistence do DB. Dla kazdego meetingu
//       (identyfikowanego przez meeting_id) trzymamy liste temp speakerow z
//       embeddingami i labelami SPEAKER_XX. Stan jest persystowany do tabeli
//       voice_temp_speakers zeby po zakonczeniu meetingu LLM mogl zrobic
//       post-processing ("SPEAKER_01 = Jan Kowalski") i przeniesc embeddingi
//       do voice_profile_samples.
//
//       Cluster assignment: dla nowego embeddingu porownujemy z wszystkimi
//       embeddingami w oknie kazdego temp speakera, bierzemy speakera o
//       najwyzszej max-similarity. Jesli przekracza threshold → match,
//       inaczej → nowy SPEAKER_XX.
// =============================================================================

use super::embedding::cosine_similarity;
use super::voice_profile::{bytes_to_embedding, embedding_to_bytes};
use crate::db::models::DbVoiceTempSpeaker;
use crate::db::{repository as repo, DbPool};
use anyhow::Result;
use std::collections::VecDeque;

/// Maksymalna liczba ostatnich embeddingow przechowywanych per mowca
/// w pamieci trackera. Wszystkie embeddingi sa jednak zapisywane do DB.
pub const EMBEDDINGS_PER_SPEAKER: usize = 8;

/// Struktura jednego temp speakera w pamieci.
#[derive(Debug, Clone)]
struct MeetingSpeaker {
    /// DB row id (po pierwszym flush). None oznacza ze jeszcze nie zapisany.
    db_id: Option<i64>,
    /// Label typu "SPEAKER_00"
    label: String,
    /// Okno ostatnich embeddingow (L2-znormalizowane)
    recent: VecDeque<Vec<f32>>,
    /// WSZYSTKIE embeddingi uzyskane dla tego speakera w tym meetingu —
    /// sa flushowane do DB przy flush_to_db() zeby LLM mogl ich uzyc
    /// do post-meetingowego enrollment.
    all_embeddings: Vec<Vec<f32>>,
    /// Total duration wszystkich utterance tego speakera (ms)
    total_duration_ms: u64,
    /// Liczba matchow
    count: usize,
}

/// Format BLOB dla voice_temp_speakers.embeddings_blob:
/// [u32 count (LE)][count * 192 * f32 LE bytes]
/// Prosty, szybki, deterministyczny.
fn encode_embeddings_blob(embeddings: &[Vec<f32>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + embeddings.len() * 192 * 4);
    out.extend_from_slice(&(embeddings.len() as u32).to_le_bytes());
    for emb in embeddings {
        out.extend_from_slice(&embedding_to_bytes(emb));
    }
    out
}

fn decode_embeddings_blob(blob: &[u8]) -> Vec<Vec<f32>> {
    if blob.len() < 4 {
        return Vec::new();
    }
    let count = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]) as usize;
    let expected = 4 + count * 192 * 4;
    if blob.len() < expected {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let start = 4 + i * 192 * 4;
        let end = start + 192 * 4;
        out.push(bytes_to_embedding(&blob[start..end]));
    }
    out
}

/// Per-meeting speaker tracker. Kazdy meeting ma swoj instancje.
pub struct MeetingSpeakerTracker {
    meeting_id: String,
    speakers: Vec<MeetingSpeaker>,
    similarity_threshold: f32,
    max_speakers: usize,
}

/// Wynik track — label + czy to nowy speaker
#[derive(Debug, Clone)]
pub struct TrackResult {
    pub label: String,
    pub is_new_speaker: bool,
    pub similarity: f32,
}

impl MeetingSpeakerTracker {
    /// Tworzy nowy tracker dla meetingu. Jesli meeting_id juz byl w DB —
    /// laduje istniejacy stan (np. bot sie re-polaczyl w trakcie).
    pub fn load_or_new(
        pool: &DbPool,
        meeting_id: &str,
        similarity_threshold: f32,
        max_speakers: usize,
    ) -> Result<Self> {
        let existing: Vec<DbVoiceTempSpeaker> = repo::list_voice_temp_speakers(pool, meeting_id)?;
        let speakers: Vec<MeetingSpeaker> = existing
            .into_iter()
            .map(|row| {
                let all_embeddings = decode_embeddings_blob(&row.embeddings_blob);
                let recent: VecDeque<Vec<f32>> = all_embeddings
                    .iter()
                    .rev()
                    .take(EMBEDDINGS_PER_SPEAKER)
                    .rev()
                    .cloned()
                    .collect();
                MeetingSpeaker {
                    db_id: Some(row.id),
                    label: row.temp_label,
                    recent,
                    all_embeddings,
                    total_duration_ms: row.total_duration_ms as u64,
                    count: row.sample_count as usize,
                }
            })
            .collect();

        tracing::info!(
            meeting_id = %meeting_id,
            loaded_speakers = speakers.len(),
            "Meeting tracker załadowany z DB"
        );

        Ok(Self {
            meeting_id: meeting_id.to_string(),
            speakers,
            similarity_threshold,
            max_speakers,
        })
    }

    pub fn meeting_id(&self) -> &str {
        &self.meeting_id
    }

    pub fn speaker_count(&self) -> usize {
        self.speakers.len()
    }

    /// Dopasowuje embedding do istniejacego speakera albo tworzy nowego.
    /// Zapisuje zmianę do DB natychmiast (nie trzeba wołać flush).
    pub fn track(
        &mut self,
        pool: &DbPool,
        embedding: &[f32],
        duration_ms: u64,
    ) -> Result<TrackResult> {
        let normalized = l2_normalize(embedding);

        let mut best_idx: Option<usize> = None;
        let mut best_sim: f32 = -1.0;

        for (i, spk) in self.speakers.iter().enumerate() {
            let max_sim_for_speaker = spk
                .recent
                .iter()
                .map(|e| cosine_similarity(&normalized, e))
                .fold(f32::NEG_INFINITY, f32::max);
            if max_sim_for_speaker > best_sim {
                best_sim = max_sim_for_speaker;
                best_idx = Some(i);
            }
        }

        let (label, is_new_speaker, idx) = match best_idx {
            Some(idx) if best_sim >= self.similarity_threshold => {
                let spk = &mut self.speakers[idx];
                spk.recent.push_back(normalized.clone());
                while spk.recent.len() > EMBEDDINGS_PER_SPEAKER {
                    spk.recent.pop_front();
                }
                spk.all_embeddings.push(normalized);
                spk.count += 1;
                spk.total_duration_ms += duration_ms;
                tracing::debug!(
                    meeting_id = %self.meeting_id,
                    speaker = %spk.label,
                    similarity = best_sim,
                    count = spk.count,
                    "Speaker matched"
                );
                (spk.label.clone(), false, idx)
            }
            _ if self.speakers.len() < self.max_speakers => {
                let label = format!("SPEAKER_{:02}", self.speakers.len());
                let mut recent = VecDeque::with_capacity(EMBEDDINGS_PER_SPEAKER);
                recent.push_back(normalized.clone());
                self.speakers.push(MeetingSpeaker {
                    db_id: None,
                    label: label.clone(),
                    recent,
                    all_embeddings: vec![normalized],
                    total_duration_ms: duration_ms,
                    count: 1,
                });
                tracing::info!(
                    meeting_id = %self.meeting_id,
                    speaker = %label,
                    best_sim_to_existing = best_sim,
                    "Nowy temp speaker utworzony"
                );
                (label, true, self.speakers.len() - 1)
            }
            Some(idx) => {
                // Limit osiagniety — forced match do najblizszego
                let spk = &mut self.speakers[idx];
                spk.recent.push_back(normalized.clone());
                while spk.recent.len() > EMBEDDINGS_PER_SPEAKER {
                    spk.recent.pop_front();
                }
                spk.all_embeddings.push(normalized);
                spk.count += 1;
                spk.total_duration_ms += duration_ms;
                tracing::debug!(
                    meeting_id = %self.meeting_id,
                    speaker = %spk.label,
                    similarity = best_sim,
                    "Speaker matched (max_speakers reached, forced)"
                );
                (spk.label.clone(), false, idx)
            }
            None => {
                // Pusty tracker + limit 0 — szybki fallback
                return Ok(TrackResult {
                    label: "SPEAKER_UNKNOWN".to_string(),
                    is_new_speaker: false,
                    similarity: 0.0,
                });
            }
        };

        // Zapisz do DB natychmiast (upsert)
        self.persist_speaker(pool, idx)?;

        Ok(TrackResult {
            label,
            is_new_speaker,
            similarity: best_sim.max(0.0),
        })
    }

    /// Zapisuje (albo aktualizuje) pojedynczego speakera do DB.
    fn persist_speaker(&mut self, pool: &DbPool, idx: usize) -> Result<()> {
        let spk = &self.speakers[idx];
        let blob = encode_embeddings_blob(&spk.all_embeddings);
        let id = repo::upsert_voice_temp_speaker(
            pool,
            &self.meeting_id,
            &spk.label,
            &blob,
            spk.count as i64,
            spk.total_duration_ms as i64,
        )?;
        self.speakers[idx].db_id = Some(id);
        Ok(())
    }

    /// Flush wszystkich speakerow do DB (np. przy leave_meeting dla pewnosci).
    pub fn flush_all(&mut self, pool: &DbPool) -> Result<()> {
        for i in 0..self.speakers.len() {
            self.persist_speaker(pool, i)?;
        }
        Ok(())
    }

    /// Zwraca snapshot obecnych temp speakerow (dla diagnostyki/API).
    pub fn snapshot(&self) -> Vec<TempSpeakerSnapshot> {
        self.speakers
            .iter()
            .map(|s| TempSpeakerSnapshot {
                label: s.label.clone(),
                sample_count: s.count,
                total_duration_ms: s.total_duration_ms,
                db_id: s.db_id,
            })
            .collect()
    }
}

/// Lekki snapshot temp speakera dla API
#[derive(Debug, Clone, serde::Serialize)]
pub struct TempSpeakerSnapshot {
    pub label: String,
    pub sample_count: usize,
    pub total_duration_ms: u64,
    pub db_id: Option<i64>,
}

fn l2_normalize(v: &[f32]) -> Vec<f32> {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm < 1e-12 {
        return v.to_vec();
    }
    v.iter().map(|x| x / norm).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex};

    fn test_pool() -> DbPool {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn dummy_emb(seed: f32) -> Vec<f32> {
        (0..192).map(|i| ((i as f32) * 0.01 + seed).sin()).collect()
    }

    #[test]
    fn new_tracker_is_empty() {
        let pool = test_pool();
        let tracker = MeetingSpeakerTracker::load_or_new(&pool, "meet-1", 0.5, 10).unwrap();
        assert_eq!(tracker.speaker_count(), 0);
    }

    #[test]
    fn track_creates_speaker_and_persists() {
        let pool = test_pool();
        let mut tracker = MeetingSpeakerTracker::load_or_new(&pool, "meet-1", 0.5, 10).unwrap();

        let result = tracker.track(&pool, &dummy_emb(0.0), 3000).unwrap();
        assert_eq!(result.label, "SPEAKER_00");
        assert!(result.is_new_speaker);

        // Sprawdz w DB
        let temps = repo::list_voice_temp_speakers(&pool, "meet-1").unwrap();
        assert_eq!(temps.len(), 1);
        assert_eq!(temps[0].temp_label, "SPEAKER_00");
        assert_eq!(temps[0].sample_count, 1);
        assert_eq!(temps[0].total_duration_ms, 3000);
    }

    #[test]
    fn track_similar_matches_same_speaker() {
        let pool = test_pool();
        let mut tracker = MeetingSpeakerTracker::load_or_new(&pool, "meet-1", 0.5, 10).unwrap();

        tracker.track(&pool, &dummy_emb(0.0), 3000).unwrap();
        let r2 = tracker.track(&pool, &dummy_emb(0.0), 3000).unwrap();
        assert_eq!(r2.label, "SPEAKER_00");
        assert!(!r2.is_new_speaker);

        assert_eq!(tracker.speaker_count(), 1);
    }

    #[test]
    fn track_different_creates_new() {
        let pool = test_pool();
        let mut tracker = MeetingSpeakerTracker::load_or_new(&pool, "meet-1", 0.9, 10).unwrap();

        let r1 = tracker.track(&pool, &vec![1.0, 0.0, 0.0], 3000).unwrap();
        assert_eq!(r1.label, "SPEAKER_00");
        // Wymus niski cos similarity
        let emb_diff: Vec<f32> = {
            let mut v = vec![0.0; 192];
            v[100] = 1.0;
            v
        };
        let r2 = tracker.track(&pool, &emb_diff, 3000).unwrap();
        assert_eq!(r2.label, "SPEAKER_01");

        assert_eq!(tracker.speaker_count(), 2);
    }

    #[test]
    fn load_after_save_restores_state() {
        let pool = test_pool();
        // Dwa naprawde rozne embeddingi (orthogonal) — zeby tracker zrobil
        // dwoch speakerow, nie jednego z ktorego potem nie odzyskamy danych.
        let emb_a: Vec<f32> = {
            let mut v = vec![0.0_f32; 192];
            v[0] = 1.0;
            v
        };
        let emb_b: Vec<f32> = {
            let mut v = vec![0.0_f32; 192];
            v[100] = 1.0;
            v
        };
        {
            let mut tracker =
                MeetingSpeakerTracker::load_or_new(&pool, "meet-2", 0.5, 10).unwrap();
            tracker.track(&pool, &emb_a, 3000).unwrap();
            tracker.track(&pool, &emb_a, 2000).unwrap();
            tracker.track(&pool, &emb_b, 3000).unwrap();
            tracker.flush_all(&pool).unwrap();
        }

        // Reload — powinno byc 2 speakerow z zachowanymi counts
        let loaded = MeetingSpeakerTracker::load_or_new(&pool, "meet-2", 0.5, 10).unwrap();
        assert_eq!(loaded.speaker_count(), 2);
        let snaps = loaded.snapshot();
        let s0 = snaps.iter().find(|s| s.label == "SPEAKER_00").unwrap();
        assert_eq!(s0.sample_count, 2);
        assert_eq!(s0.total_duration_ms, 5000);
        let s1 = snaps.iter().find(|s| s.label == "SPEAKER_01").unwrap();
        assert_eq!(s1.sample_count, 1);
    }

    #[test]
    fn encode_decode_blob_roundtrip() {
        let embs = vec![dummy_emb(0.0), dummy_emb(1.0), dummy_emb(2.0)];
        let blob = encode_embeddings_blob(&embs);
        let decoded = decode_embeddings_blob(&blob);
        assert_eq!(decoded.len(), 3);
        for (a, b) in embs.iter().zip(decoded.iter()) {
            for (x, y) in a.iter().zip(b.iter()) {
                assert!((x - y).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn max_speakers_enforces_limit() {
        let pool = test_pool();
        let mut tracker = MeetingSpeakerTracker::load_or_new(&pool, "meet-1", 0.95, 2).unwrap();

        let r1 = tracker.track(&pool, &{
            let mut v = vec![0.0_f32; 192];
            v[0] = 1.0;
            v
        }, 3000).unwrap();
        let r2 = tracker.track(&pool, &{
            let mut v = vec![0.0_f32; 192];
            v[50] = 1.0;
            v
        }, 3000).unwrap();
        let r3 = tracker.track(&pool, &{
            let mut v = vec![0.0_f32; 192];
            v[100] = 1.0;
            v
        }, 3000).unwrap();

        assert_eq!(r1.label, "SPEAKER_00");
        assert_eq!(r2.label, "SPEAKER_01");
        // Trzeci forced na najblizszego
        assert!(r3.label == "SPEAKER_00" || r3.label == "SPEAKER_01");
        assert_eq!(tracker.speaker_count(), 2);
    }
}
