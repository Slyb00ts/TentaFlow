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

// =============================================================================
// Kryteria auto-promocji temp speaker → KNOWN_SPEAKER voice_profile
// =============================================================================

/// Minimalna dlugosc sample zeby zakwalifikowal sie do promocji (ms).
/// Krotsze wypowiedzi (<2s) daja niestabilne embeddingi.
pub const PROMOTION_SAMPLE_MIN_DURATION_MS: u64 = 2000;

/// Minimalny SNR sample (dB) — filtruje szum, oddechy, krzyki.
pub const PROMOTION_SAMPLE_MIN_SNR_DB: f32 = 12.0;

/// Minimalna liczba kwalifikujacych sie sample (>= min_duration, >= min_snr).
pub const PROMOTION_MIN_QUALITY_SAMPLES: usize = 5;

/// Minimalna suma duration kwalifikujacych sie sample (ms).
pub const PROMOTION_MIN_TOTAL_DURATION_MS: u64 = 15_000;

/// Minimalna wewnetrzna cos similarity miedzy kwalifikujacymi sie sample.
/// Musi odpowiadac voice_profile::MIN_INTRA_SIMILARITY — bo inaczej tracker
/// akceptuje samples ktore potem enroll_profile odrzuca (promotion fail).
/// Niski prog 0.30: sprawdzane TOP-K najlepszych sample po wstepnym dropout
/// outlierow w promote_speaker, nie wszystkie quality samples.
pub const PROMOTION_MIN_INTRA_SIMILARITY: f32 = 0.50;

/// Sample w trackerze — embedding + metadata uzywane do decyzji promocyjnej.
#[derive(Debug, Clone)]
pub struct TrackedSample {
    /// L2-znormalizowany embedding [192 × f32]
    pub embedding: Vec<f32>,
    /// Dlugosc oryginalnego audio z ktorego wyciagniety (ms)
    pub duration_ms: u64,
    /// Szacowane SNR (dB) — wyzsze = czystszy sygnal
    pub snr_db: f32,
}

impl TrackedSample {
    /// Czy sample spelnia kryteria quality dla promocji
    fn is_quality(&self) -> bool {
        self.duration_ms >= PROMOTION_SAMPLE_MIN_DURATION_MS
            && self.snr_db >= PROMOTION_SAMPLE_MIN_SNR_DB
    }
}

/// Struktura jednego temp speakera w pamieci.
#[derive(Debug, Clone)]
struct MeetingSpeaker {
    /// DB row id (po pierwszym flush). None oznacza ze jeszcze nie zapisany.
    db_id: Option<i64>,
    /// Label typu "SPEAKER_00"
    label: String,
    /// Okno ostatnich embeddingow (L2-znormalizowane) — szybki matching
    recent: VecDeque<Vec<f32>>,
    /// WSZYSTKIE samples uzyskane dla tego speakera w tym meetingu (z metadata).
    /// Persystowane do DB, uzywane przy promocji jako zrodlo voice_profile_samples.
    all_samples: Vec<TrackedSample>,
    /// Total duration wszystkich utterance tego speakera (ms)
    total_duration_ms: u64,
    /// Liczba matchow
    count: usize,
}

impl MeetingSpeaker {
    /// Zwraca liste indeksow kwalifikujacych sie sample (quality gates)
    fn quality_sample_indices(&self) -> Vec<usize> {
        self.all_samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.is_quality())
            .map(|(i, _)| i)
            .collect()
    }

    /// Sprawdza czy speaker jest gotowy do auto-promocji do voice_profile.
    /// Zwraca Ok(indeksy) jesli tak, Err(powod) jesli nie.
    fn promotion_candidates(&self) -> Result<Vec<usize>, PromotionReject> {
        let quality = self.quality_sample_indices();
        if quality.len() < PROMOTION_MIN_QUALITY_SAMPLES {
            return Err(PromotionReject::NotEnoughQualitySamples {
                got: quality.len(),
                need: PROMOTION_MIN_QUALITY_SAMPLES,
            });
        }
        let total_quality_duration: u64 = quality
            .iter()
            .map(|&i| self.all_samples[i].duration_ms)
            .sum();
        if total_quality_duration < PROMOTION_MIN_TOTAL_DURATION_MS {
            return Err(PromotionReject::NotEnoughTotalDuration {
                got_ms: total_quality_duration,
                need_ms: PROMOTION_MIN_TOTAL_DURATION_MS,
            });
        }
        let embeddings: Vec<&Vec<f32>> = quality
            .iter()
            .map(|&i| &self.all_samples[i].embedding)
            .collect();
        let intra = intra_similarity_refs(&embeddings);
        if intra < PROMOTION_MIN_INTRA_SIMILARITY {
            return Err(PromotionReject::IntraSimilarityTooLow {
                got: intra,
                need: PROMOTION_MIN_INTRA_SIMILARITY,
            });
        }
        Ok(quality)
    }
}

/// Powod odrzucenia auto-promocji — uzywany do diagnostycznego logowania.
#[derive(Debug)]
enum PromotionReject {
    NotEnoughQualitySamples { got: usize, need: usize },
    NotEnoughTotalDuration { got_ms: u64, need_ms: u64 },
    IntraSimilarityTooLow { got: f32, need: f32 },
}

impl std::fmt::Display for PromotionReject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotEnoughQualitySamples { got, need } => {
                write!(f, "quality_samples={}/{}", got, need)
            }
            Self::NotEnoughTotalDuration { got_ms, need_ms } => {
                write!(f, "total_duration_ms={}/{}", got_ms, need_ms)
            }
            Self::IntraSimilarityTooLow { got, need } => {
                write!(f, "intra_similarity={:.3}/{:.3}", got, need)
            }
        }
    }
}

/// Wybiera k sample ktorych embedding jest najblizszy centroidowi grupy.
/// Uzywane przy promocji: drop outlierow zanim wyslemy do enroll_profile, zeby
/// jeden dziwny sample nie zepsul intra-similarity calego profilu.
/// Zwraca indeksy w kolejnosci wzrostu (stabilna, deterministyczna).
fn select_best_k_by_centroid(
    all_samples: &[TrackedSample],
    candidate_indices: &[usize],
    k: usize,
) -> Vec<usize> {
    if candidate_indices.len() <= k {
        return candidate_indices.to_vec();
    }
    let dim = all_samples[candidate_indices[0]].embedding.len();
    let mut centroid = vec![0.0_f32; dim];
    for &i in candidate_indices {
        let emb = &all_samples[i].embedding;
        for (c, e) in centroid.iter_mut().zip(emb.iter()) {
            *c += *e;
        }
    }
    let n = candidate_indices.len() as f32;
    for c in centroid.iter_mut() {
        *c /= n;
    }
    let mut scored: Vec<(usize, f32)> = candidate_indices
        .iter()
        .map(|&i| (i, cosine_similarity(&all_samples[i].embedding, &centroid)))
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    let mut result: Vec<usize> = scored.into_iter().map(|(i, _)| i).collect();
    result.sort_unstable();
    result
}

/// Helper: liczy srednia cos similarity miedzy samples (dla refs — zero-copy).
fn intra_similarity_refs(samples: &[&Vec<f32>]) -> f32 {
    if samples.len() < 2 {
        return 1.0;
    }
    let mut sum = 0.0_f32;
    let mut count = 0;
    for i in 0..samples.len() {
        for j in (i + 1)..samples.len() {
            sum += cosine_similarity(samples[i], samples[j]);
            count += 1;
        }
    }
    if count == 0 {
        1.0
    } else {
        sum / count as f32
    }
}

/// Format BLOB voice_temp_speakers.embeddings_blob — wersja 2.
///
///   [u32 magic = 0xFFFFFFFF][u32 version = 2][u32 count]
///   [count × {192 × f32 LE embedding + u32 LE duration_ms + f32 LE snr_db}]
///
/// Magic 0xFFFFFFFF odrozniamy od starej wersji (v1 pierwszy u32 to count,
/// ktory nigdy nie byl 0xFFFFFFFF). Przy odczycie wykrywamy wersje automatycznie.
const BLOB_MAGIC_V2: u32 = 0xFFFFFFFF;
const SAMPLE_SIZE_V2: usize = 192 * 4 + 4 + 4; // embedding + duration + snr

fn encode_samples_blob(samples: &[TrackedSample]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + samples.len() * SAMPLE_SIZE_V2);
    out.extend_from_slice(&BLOB_MAGIC_V2.to_le_bytes());
    out.extend_from_slice(&2_u32.to_le_bytes());
    out.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    for s in samples {
        out.extend_from_slice(&embedding_to_bytes(&s.embedding));
        out.extend_from_slice(&(s.duration_ms as u32).to_le_bytes());
        out.extend_from_slice(&s.snr_db.to_le_bytes());
    }
    out
}

fn decode_samples_blob(blob: &[u8]) -> Vec<TrackedSample> {
    if blob.len() < 4 {
        return Vec::new();
    }
    let first = u32::from_le_bytes([blob[0], blob[1], blob[2], blob[3]]);
    if first == BLOB_MAGIC_V2 {
        // v2 format
        if blob.len() < 12 {
            return Vec::new();
        }
        let version = u32::from_le_bytes([blob[4], blob[5], blob[6], blob[7]]);
        if version != 2 {
            tracing::warn!(version, "Unsupported blob version, treating as empty");
            return Vec::new();
        }
        let count = u32::from_le_bytes([blob[8], blob[9], blob[10], blob[11]]) as usize;
        let expected = 12 + count * SAMPLE_SIZE_V2;
        if blob.len() < expected {
            tracing::warn!(blob_len = blob.len(), expected, "Blob too short");
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let base = 12 + i * SAMPLE_SIZE_V2;
            let emb = bytes_to_embedding(&blob[base..base + 192 * 4]);
            let dur = u32::from_le_bytes([
                blob[base + 192 * 4],
                blob[base + 192 * 4 + 1],
                blob[base + 192 * 4 + 2],
                blob[base + 192 * 4 + 3],
            ]) as u64;
            let snr = f32::from_le_bytes([
                blob[base + 192 * 4 + 4],
                blob[base + 192 * 4 + 5],
                blob[base + 192 * 4 + 6],
                blob[base + 192 * 4 + 7],
            ]);
            out.push(TrackedSample {
                embedding: emb,
                duration_ms: dur,
                snr_db: snr,
            });
        }
        out
    } else {
        // Legacy v1 format: [u32 count][count × 192 × f32]
        // (Nie ma tu metadata, wiec wrappujemy z zero SNR/duration — te samples
        // NIE sa kwalifikowalne do promocji, co jest OK — user robiac meeting
        // po upgrade mial juz upgradowany kod).
        let count = first as usize;
        let expected = 4 + count * 192 * 4;
        if blob.len() < expected {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(count);
        for i in 0..count {
            let start = 4 + i * 192 * 4;
            let emb = bytes_to_embedding(&blob[start..start + 192 * 4]);
            out.push(TrackedSample {
                embedding: emb,
                duration_ms: 0,
                snr_db: 0.0,
            });
        }
        out
    }
}

/// Per-meeting speaker tracker. Kazdy meeting ma swoj instancje.
pub struct MeetingSpeakerTracker {
    meeting_id: String,
    speakers: Vec<MeetingSpeaker>,
    similarity_threshold: f32,
    max_speakers: usize,
}

/// Wynik track — label + czy to nowy speaker + czy nastapila promocja
#[derive(Debug, Clone)]
pub struct TrackResult {
    pub label: String,
    pub is_new_speaker: bool,
    pub similarity: f32,
    /// True jesli w ramach tego track() temp speaker zostal auto-promowany
    /// do voice_profile (KNOWN_SPEAKER_XX). Caller powinien re-matchnac przez
    /// enrolled profiles zeby uzyskac nowa etykiete juz dla tej wypowiedzi.
    pub promoted: bool,
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
            .filter(|row| row.assigned_profile_id.is_none()) // pomin juz promowanych
            .map(|row| {
                let all_samples = decode_samples_blob(&row.embeddings_blob);
                let recent: VecDeque<Vec<f32>> = all_samples
                    .iter()
                    .rev()
                    .take(EMBEDDINGS_PER_SPEAKER)
                    .rev()
                    .map(|s| s.embedding.clone())
                    .collect();
                MeetingSpeaker {
                    db_id: Some(row.id),
                    label: row.temp_label,
                    recent,
                    all_samples,
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
    ///
    /// Zwraca TrackResult ze informacja czy nastapila promocja — wtedy caller
    /// moze re-matchnac przez match_to_profiles zeby uzyskac nowa etykiete
    /// (np. "KNOWN_SPEAKER_01") juz dla tej samej wypowiedzi.
    pub fn track(
        &mut self,
        pool: &DbPool,
        embedding: &[f32],
        duration_ms: u64,
        snr_db: f32,
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

        let sample = TrackedSample {
            embedding: normalized.clone(),
            duration_ms,
            snr_db,
        };

        let (label, is_new_speaker, idx) = match best_idx {
            Some(idx) if best_sim >= self.similarity_threshold => {
                let spk = &mut self.speakers[idx];
                spk.recent.push_back(normalized);
                while spk.recent.len() > EMBEDDINGS_PER_SPEAKER {
                    spk.recent.pop_front();
                }
                spk.all_samples.push(sample);
                spk.count += 1;
                spk.total_duration_ms += duration_ms;
                tracing::debug!(
                    meeting_id = %self.meeting_id,
                    speaker = %spk.label,
                    similarity = best_sim,
                    count = spk.count,
                    quality_samples = spk.quality_sample_indices().len(),
                    "Speaker matched"
                );
                (spk.label.clone(), false, idx)
            }
            _ if self.speakers.len() < self.max_speakers => {
                let label = self.next_speaker_label();
                let mut recent = VecDeque::with_capacity(EMBEDDINGS_PER_SPEAKER);
                recent.push_back(normalized);
                self.speakers.push(MeetingSpeaker {
                    db_id: None,
                    label: label.clone(),
                    recent,
                    all_samples: vec![sample],
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
                spk.recent.push_back(normalized);
                while spk.recent.len() > EMBEDDINGS_PER_SPEAKER {
                    spk.recent.pop_front();
                }
                spk.all_samples.push(sample);
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
                return Ok(TrackResult {
                    label: "SPEAKER_UNKNOWN".to_string(),
                    is_new_speaker: false,
                    similarity: 0.0,
                    promoted: false,
                });
            }
        };

        self.persist_speaker(pool, idx)?;

        // Diagnostyka co dokladnie sie dzieje z tym sample (INFO zeby bylo widac
        // bez wlaczania debug). Wazne dla strojenia progow promocji.
        {
            let spk = &self.speakers[idx];
            let last = spk.all_samples.last();
            let (last_dur, last_snr, last_quality) = match last {
                Some(s) => (s.duration_ms, s.snr_db, s.is_quality()),
                None => (0, 0.0, false),
            };
            let quality_count = spk.quality_sample_indices().len();
            tracing::info!(
                meeting_id = %self.meeting_id,
                speaker = %label,
                similarity = best_sim,
                count = spk.count,
                total_duration_ms = spk.total_duration_ms,
                sample_duration_ms = last_dur,
                sample_snr_db = last_snr,
                sample_quality = last_quality,
                quality_samples = quality_count,
                need_quality = PROMOTION_MIN_QUALITY_SAMPLES,
                "Sample tracked"
            );
        }

        // Po persist — sprawdz czy ten speaker dorobil sie na promocje
        let mut promoted = false;
        match self.speakers[idx].promotion_candidates() {
            Ok(quality_idx_list) => match self.promote_speaker(pool, idx, &quality_idx_list) {
                Ok(Some(profile_id)) => {
                    tracing::info!(
                        meeting_id = %self.meeting_id,
                        profile_id,
                        previous_label = %label,
                        "Temp speaker promoted to KNOWN_SPEAKER voice_profile"
                    );
                    promoted = true;
                    self.speakers.remove(idx);
                }
                Ok(None) => {
                    tracing::warn!(
                        meeting_id = %self.meeting_id,
                        speaker = %label,
                        "Promotion check passed but enroll_profile returned None"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = ?e, "Promotion failed");
                }
            },
            Err(reject) => {
                tracing::info!(
                    meeting_id = %self.meeting_id,
                    speaker = %label,
                    reason = %reject,
                    "Not yet ready for promotion"
                );
            }
        }

        Ok(TrackResult {
            label,
            is_new_speaker,
            similarity: best_sim.max(0.0),
            promoted,
        })
    }

    /// Zwraca nastepny wolny label dla temp speakera — omija te
    /// ktore juz zostaly promowane (zeby nie dostac SPEAKER_00 dwa razy).
    fn next_speaker_label(&self) -> String {
        let mut used: Vec<usize> = self
            .speakers
            .iter()
            .filter_map(|s| {
                s.label
                    .strip_prefix("SPEAKER_")
                    .and_then(|n| n.parse::<usize>().ok())
            })
            .collect();
        used.sort_unstable();
        let mut next = 0;
        for u in &used {
            if *u == next {
                next += 1;
            } else {
                break;
            }
        }
        format!("SPEAKER_{:02}", next)
    }

    /// Promuje temp speakera do voice_profile jako "KNOWN_SPEAKER_XX".
    /// Zwraca id utworzonego profilu albo None jesli promocja sie nie udala.
    fn promote_speaker(
        &self,
        pool: &DbPool,
        spk_idx: usize,
        quality_indices: &[usize],
    ) -> Result<Option<i64>> {
        let spk = &self.speakers[spk_idx];

        // Pick top-K sampli najbardziej spojnych z centroidem — drop outlierow.
        // WeSpeaker dla niektorych glosow daje wariancje ~0.15-0.20 miedzy
        // utterance, wiec nawet same-speaker moze miec jeden odstajacy sample.
        // Wybieramy PROMOTION_MIN_QUALITY_SAMPLES najblizszych centroidu.
        let best_indices = select_best_k_by_centroid(
            &spk.all_samples,
            quality_indices,
            PROMOTION_MIN_QUALITY_SAMPLES,
        );

        let quality_samples: Vec<crate::diarization::voice_profile::EnrollmentSample> =
            best_indices
                .iter()
                .map(|&i| {
                    let s = &spk.all_samples[i];
                    crate::diarization::voice_profile::EnrollmentSample {
                        embedding: s.embedding.clone(),
                        duration_ms: s.duration_ms,
                        snr_db: s.snr_db,
                        rms: 0.0,
                    }
                })
                .collect();

        // Wylicz nowy numer KNOWN_SPEAKER
        let next_num = repo::next_known_speaker_number(pool)?;
        let name = format!("KNOWN_SPEAKER_{:02}", next_num);
        let identity = crate::diarization::voice_profile::PersonIdentity::new(&name);

        match crate::diarization::voice_profile::enroll_profile(
            pool,
            &identity,
            &quality_samples,
            "auto_promoted",
        ) {
            Ok(result) => {
                // Oznacz temp speakera jako przypisanego do profilu (audit trail)
                if let Some(temp_id) = spk.db_id {
                    repo::assign_temp_speaker_to_profile(pool, temp_id, result.profile_id).ok();
                }
                Ok(Some(result.profile_id))
            }
            Err(e) => {
                tracing::warn!(error = ?e, "enroll_profile failed during promotion");
                Ok(None)
            }
        }
    }

    /// Zapisuje (albo aktualizuje) pojedynczego speakera do DB.
    fn persist_speaker(&mut self, pool: &DbPool, idx: usize) -> Result<()> {
        let spk = &self.speakers[idx];
        let blob = encode_samples_blob(&spk.all_samples);
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
                quality_samples: s.quality_sample_indices().len(),
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
    /// Ile z `sample_count` spelnia kryteria quality dla promocji.
    /// Gdy >= PROMOTION_MIN_QUALITY_SAMPLES → gotowy do auto-promocji.
    pub quality_samples: usize,
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

        let result = tracker.track(&pool, &dummy_emb(0.0), 3000, 20.0).unwrap();
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

        tracker.track(&pool, &dummy_emb(0.0), 3000, 20.0).unwrap();
        let r2 = tracker.track(&pool, &dummy_emb(0.0), 3000, 20.0).unwrap();
        assert_eq!(r2.label, "SPEAKER_00");
        assert!(!r2.is_new_speaker);

        assert_eq!(tracker.speaker_count(), 1);
    }

    #[test]
    fn track_different_creates_new() {
        let pool = test_pool();
        let mut tracker = MeetingSpeakerTracker::load_or_new(&pool, "meet-1", 0.9, 10).unwrap();

        let r1 = tracker
            .track(&pool, &vec![1.0, 0.0, 0.0], 3000, 20.0)
            .unwrap();
        assert_eq!(r1.label, "SPEAKER_00");
        // Wymus niski cos similarity
        let emb_diff: Vec<f32> = {
            let mut v = vec![0.0; 192];
            v[100] = 1.0;
            v
        };
        let r2 = tracker.track(&pool, &emb_diff, 3000, 20.0).unwrap();
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
            let mut tracker = MeetingSpeakerTracker::load_or_new(&pool, "meet-2", 0.5, 10).unwrap();
            tracker.track(&pool, &emb_a, 3000, 20.0).unwrap();
            tracker.track(&pool, &emb_a, 2000, 20.0).unwrap();
            tracker.track(&pool, &emb_b, 3000, 20.0).unwrap();
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
    fn encode_decode_blob_v2_roundtrip() {
        let samples = vec![
            TrackedSample {
                embedding: dummy_emb(0.0),
                duration_ms: 3000,
                snr_db: 18.5,
            },
            TrackedSample {
                embedding: dummy_emb(1.0),
                duration_ms: 2500,
                snr_db: 22.1,
            },
            TrackedSample {
                embedding: dummy_emb(2.0),
                duration_ms: 4000,
                snr_db: 15.0,
            },
        ];
        let blob = encode_samples_blob(&samples);
        let decoded = decode_samples_blob(&blob);
        assert_eq!(decoded.len(), 3);
        for (a, b) in samples.iter().zip(decoded.iter()) {
            assert_eq!(a.duration_ms, b.duration_ms);
            assert!((a.snr_db - b.snr_db).abs() < 1e-5);
            for (x, y) in a.embedding.iter().zip(b.embedding.iter()) {
                assert!((x - y).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn decode_blob_v1_legacy_format() {
        // v1: [u32 count][count × 192 × f32] — bez metadata
        let count: u32 = 2;
        let mut blob = Vec::new();
        blob.extend_from_slice(&count.to_le_bytes());
        for s in &[dummy_emb(0.0), dummy_emb(1.0)] {
            for v in s {
                blob.extend_from_slice(&v.to_le_bytes());
            }
        }
        let decoded = decode_samples_blob(&blob);
        assert_eq!(decoded.len(), 2);
        // Brak metadata → 0 (samples nie sa kwalifikowalne do promocji)
        assert_eq!(decoded[0].duration_ms, 0);
        assert_eq!(decoded[0].snr_db, 0.0);
    }

    #[test]
    fn auto_promotion_after_quality_samples() {
        let pool = test_pool();
        let mut tracker = MeetingSpeakerTracker::load_or_new(&pool, "meet-promo", 0.5, 10).unwrap();

        // Sample which always matches itself (constant embedding) — symuluje
        // ten sam mowca w 6 dluzszych wypowiedziach.
        let same_voice = dummy_emb(0.0);

        // 5 samples — kazdy 3s, SNR 18 → quality. Po 5 powinno byc gotowe do promocji.
        for i in 0..5 {
            let r = tracker.track(&pool, &same_voice, 3000, 18.0).unwrap();
            // Pierwsze 4 — bez promocji
            if i < 4 {
                assert!(!r.promoted, "iter {i}: promoted too early: {:?}", r);
                assert_eq!(r.label, "SPEAKER_00");
            }
        }

        // Po 5 quality samples — total 15s, intra=1.0 → spelnia kryteria.
        // Piate wywolanie powinno juz wygenerowac promocje.
        // (Sprawdzimy na 5tym wywolaniu)
        // Reset i sprawdz 5te dokladnie:
        let pool2 = test_pool();
        let mut tracker2 =
            MeetingSpeakerTracker::load_or_new(&pool2, "meet-promo-2", 0.5, 10).unwrap();
        let mut last_result = None;
        for _ in 0..5 {
            last_result = Some(tracker2.track(&pool2, &same_voice, 3000, 18.0).unwrap());
        }
        let r5 = last_result.unwrap();
        assert!(
            r5.promoted,
            "should promote on 5th quality sample, got: {:?}",
            r5
        );

        // Po promocji speaker zostal usuniety z trackera
        assert_eq!(tracker2.speaker_count(), 0);

        // Profile w voice_profiles
        let profiles = repo::list_voice_profiles(&pool2).unwrap();
        assert_eq!(profiles.len(), 1);
        assert!(profiles[0].name.starts_with("KNOWN_SPEAKER_"));
        assert_eq!(profiles[0].source, "auto_promoted");
        // Numer od 0
        assert_eq!(profiles[0].name, "KNOWN_SPEAKER_00");
    }

    #[test]
    fn promotion_skips_short_samples() {
        let pool = test_pool();
        let mut tracker = MeetingSpeakerTracker::load_or_new(&pool, "meet-short", 0.5, 10).unwrap();
        let voice = dummy_emb(0.0);

        // 10 samples ale kazdy tylko 1s → ponizej PROMOTION_SAMPLE_MIN_DURATION_MS
        for _ in 0..10 {
            let r = tracker.track(&pool, &voice, 1000, 18.0).unwrap();
            assert!(!r.promoted);
        }
        assert_eq!(tracker.speaker_count(), 1);
        let profiles = repo::list_voice_profiles(&pool).unwrap();
        assert!(profiles.is_empty());
    }

    #[test]
    fn promotion_skips_low_snr() {
        let pool = test_pool();
        let mut tracker = MeetingSpeakerTracker::load_or_new(&pool, "meet-noise", 0.5, 10).unwrap();
        let voice = dummy_emb(0.0);

        // 6 samples, dlugich, ale SNR 5 → ponizej PROMOTION_SAMPLE_MIN_SNR_DB
        for _ in 0..6 {
            let r = tracker.track(&pool, &voice, 3000, 5.0).unwrap();
            assert!(!r.promoted);
        }
        let profiles = repo::list_voice_profiles(&pool).unwrap();
        assert!(profiles.is_empty());
    }

    #[test]
    fn promotion_numbering_increments() {
        let pool = test_pool();

        // Pierwszy meeting → KNOWN_SPEAKER_00
        {
            let mut t = MeetingSpeakerTracker::load_or_new(&pool, "meet-A", 0.5, 10).unwrap();
            for _ in 0..5 {
                t.track(&pool, &dummy_emb(0.0), 3000, 18.0).unwrap();
            }
        }

        // Drugi meeting, inny glos → KNOWN_SPEAKER_01
        {
            let mut t = MeetingSpeakerTracker::load_or_new(&pool, "meet-B", 0.5, 10).unwrap();
            let other_voice: Vec<f32> = {
                let mut v = vec![0.0_f32; 192];
                v[100] = 1.0;
                v
            };
            for _ in 0..5 {
                t.track(&pool, &other_voice, 3000, 18.0).unwrap();
            }
        }

        let profiles = repo::list_voice_profiles(&pool).unwrap();
        assert_eq!(profiles.len(), 2);
        let names: std::collections::HashSet<_> = profiles.iter().map(|p| p.name.clone()).collect();
        assert!(names.contains("KNOWN_SPEAKER_00"));
        assert!(names.contains("KNOWN_SPEAKER_01"));
    }

    #[test]
    fn max_speakers_enforces_limit() {
        let pool = test_pool();
        let mut tracker = MeetingSpeakerTracker::load_or_new(&pool, "meet-1", 0.95, 2).unwrap();

        let r1 = tracker
            .track(
                &pool,
                &{
                    let mut v = vec![0.0_f32; 192];
                    v[0] = 1.0;
                    v
                },
                3000,
                20.0,
            )
            .unwrap();
        let r2 = tracker
            .track(
                &pool,
                &{
                    let mut v = vec![0.0_f32; 192];
                    v[50] = 1.0;
                    v
                },
                3000,
                20.0,
            )
            .unwrap();
        let r3 = tracker
            .track(
                &pool,
                &{
                    let mut v = vec![0.0_f32; 192];
                    v[100] = 1.0;
                    v
                },
                3000,
                20.0,
            )
            .unwrap();

        assert_eq!(r1.label, "SPEAKER_00");
        assert_eq!(r2.label, "SPEAKER_01");
        // Trzeci forced na najblizszego
        assert!(r3.label == "SPEAKER_00" || r3.label == "SPEAKER_01");
        assert_eq!(tracker.speaker_count(), 2);
    }
}
