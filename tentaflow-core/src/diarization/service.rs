// =============================================================================
// Plik: diarization/service.rs
// Opis: Service layer dla diarization — meeting-scoped tracker z persistence,
//       matching vs enrolled voice profiles, enrollment, incremental learning.
//
//       Glowny entry point:
//           identify_speaker_with_profiles(pool, pcm, meeting_id)
//       Krok po kroku:
//         1. Konwertuje PCM i16 LE → f32, clipuje do MAX_AUDIO_SAMPLES
//         2. Ekstrakcja embedding przez WeSpeaker (thread_local scratch, ~7ms)
//         3. Match vs enrolled voice_profiles (best top-K mean + centroid score)
//         4. Jesli confident match → label = profile.name, incremental learn
//         5. Jesli brak match → fallback: MeetingSpeakerTracker (DB-persisted)
//                                         generuje "SPEAKER_XX" per meeting
//       Po `leave_meeting` wolamy `end_meeting(pool, meeting_id)` zeby
//       sflushowac tracker do DB i zwolnic pamiec.
//
// UWAGA: poprzednie `identify_speaker` i `reset_tracker` zostaly USUNIETE.
// Nowa sciezka wymaga meeting_id zeby wszystko bylo w pelni audytowalne.
// =============================================================================

use super::tracker::MeetingSpeakerTracker;
use super::voice_profile::{
    self, EnrollmentResult, EnrollmentSample, PersonIdentity, ENROLL_HOP_SAMPLES,
    ENROLL_WINDOW_SAMPLES, INCREMENTAL_LEARN_THRESHOLD,
};
use super::EmbeddingExtractor;
use crate::db::DbPool;
use anyhow::Result;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::OnceLock;
use tracing::{debug, info, warn};

/// Domyslny prog online trackera — gdy embedding nie trafia do zadnego
/// enrolled profilu, dopada do temp speakera z tym progiem cosine similarity.
/// WeSpeaker same-speaker inter-utterance 0.40-0.70, cross-speaker 0.05-0.25.
/// 0.50 oddziela te rozklady — nizsze wartosci (0.30) zlepialy wszystkich
/// mowcow w jednego temp speakera.
const DEFAULT_TRACKER_THRESHOLD: f32 = 0.50;
const DEFAULT_MAX_SPEAKERS: usize = 20;

/// Minimalna dlugosc audio dla zadnego matchingu (embedding nie jest wiarygodny)
const MIN_AUDIO_SAMPLES: usize = 16000; // 1.0s @ 16kHz

/// Clipping — WeSpeaker stabilizuje sie na ~1.5s, a koszt rosnie liniowo
const MAX_AUDIO_SAMPLES: usize = 24000; // 1.5s @ 16kHz

fn default_model_path() -> String {
    std::env::var("DIARIZATION_MODEL_PATH")
        .unwrap_or_else(|_| "models/diarization/embedding.onnx".to_string())
}

/// Lazy-init WeSpeaker extractor (thread-safe, lazily loaded raz na runtime)
static EXTRACTOR: OnceLock<Option<EmbeddingExtractor>> = OnceLock::new();

fn extractor() -> Option<&'static EmbeddingExtractor> {
    EXTRACTOR
        .get_or_init(|| {
            let path = default_model_path();
            match EmbeddingExtractor::new(&path) {
                Ok(ext) => {
                    info!("Diarization extractor zaladowany z {}", path);
                    Some(ext)
                }
                Err(e) => {
                    warn!(
                        "Nie udalo sie zaladowac diarization model z {}: {} — diarization wylaczone",
                        path, e
                    );
                    None
                }
            }
        })
        .as_ref()
}

/// Mapa aktywnych trackerow per meeting_id. Kazdy tracker ma wlasny DB state.
static ACTIVE_TRACKERS: OnceLock<Mutex<HashMap<String, MeetingSpeakerTracker>>> = OnceLock::new();

fn active_trackers() -> &'static Mutex<HashMap<String, MeetingSpeakerTracker>> {
    ACTIVE_TRACKERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Rejestruje nowy meeting w trackerach — laduje istniejace temp speakers z DB
/// (jesli bot sie przelaczyl, stan jest odzyskany).
pub fn start_meeting(pool: &DbPool, meeting_id: &str) -> Result<()> {
    let tracker =
        MeetingSpeakerTracker::load_or_new(pool, meeting_id, DEFAULT_TRACKER_THRESHOLD, DEFAULT_MAX_SPEAKERS)?;
    info!(
        meeting_id = %meeting_id,
        existing_speakers = tracker.speaker_count(),
        "Meeting diarization started"
    );
    active_trackers()
        .lock()
        .insert(meeting_id.to_string(), tracker);
    Ok(())
}

/// Konczy meeting — flushuje tracker do DB i usuwa z aktywnej mapy.
/// Po tym LLM moze wywolac assign-temp-speaker na stanie z DB.
pub fn end_meeting(pool: &DbPool, meeting_id: &str) -> Result<()> {
    let mut map = active_trackers().lock();
    if let Some(mut tracker) = map.remove(meeting_id) {
        tracker.flush_all(pool)?;
        info!(
            meeting_id = %meeting_id,
            final_speakers = tracker.speaker_count(),
            "Meeting diarization finalized (flushed to DB)"
        );
    } else {
        debug!(
            meeting_id = %meeting_id,
            "end_meeting called but no active tracker — no-op"
        );
    }
    Ok(())
}

/// Wynik identyfikacji mowcy. Zawsze zawiera label (enrolled name albo
/// SPEAKER_XX). Pola profile_id/confidence wypelnione tylko przy matchu
/// do enrolled voice_profile.
#[derive(Debug, Clone)]
pub struct IdentifyResult {
    pub label: String,
    pub profile_id: Option<i64>,
    pub confidence: Option<f32>,
    pub is_enrolled: bool,
    pub is_new_temp_speaker: bool,
}

/// Identyfikuje mowce z audio w kontekscie meetingu.
///
/// Pipeline:
///   1. PCM i16 LE → f32, clipping do MAX_AUDIO_SAMPLES
///   2. WeSpeaker extract → 192-dim embedding
///   3. Match vs enrolled profiles → jesli >= threshold, zwroc imie
///   4. Fallback: meeting tracker → SPEAKER_XX z DB persistence
///   5. Incremental learning (opcjonalne, przy high confidence match)
///
/// Zwraca None gdy:
///   - model nie zaladowany
///   - audio za krotkie (< 1s)
///   - blad ekstrakcji
pub fn identify_speaker_with_profiles(
    pool: &DbPool,
    pcm_i16_le: &[u8],
    meeting_id: &str,
) -> Option<IdentifyResult> {
    let ext = extractor()?;

    // Konwersja i16 LE → f32
    let samples_f32 = voice_profile::pcm_i16_le_to_f32(pcm_i16_le);
    if samples_f32.len() < MIN_AUDIO_SAMPLES {
        debug!(
            samples = samples_f32.len(),
            min_required = MIN_AUDIO_SAMPLES,
            "identify_speaker_with_profiles — audio za krotkie"
        );
        return None;
    }

    // Clipping do srodkowych MAX_AUDIO_SAMPLES (unika ciszy na brzegach)
    let clipped: &[f32] = if samples_f32.len() > MAX_AUDIO_SAMPLES {
        let start = (samples_f32.len() - MAX_AUDIO_SAMPLES) / 2;
        &samples_f32[start..start + MAX_AUDIO_SAMPLES]
    } else {
        &samples_f32[..]
    };

    let extract_start = std::time::Instant::now();
    let embedding = match ext.extract(clipped) {
        Ok(e) => e,
        Err(e) => {
            warn!("WeSpeaker extract error: {}", e);
            return None;
        }
    };
    debug!(
        samples_in = clipped.len(),
        extract_ms = extract_start.elapsed().as_millis(),
        "WeSpeaker embedding extracted"
    );

    let audio_duration_ms = (samples_f32.len() * 1000 / 16000) as u64;

    // 1. Match vs enrolled voice profiles
    match voice_profile::match_to_profiles(pool, &embedding) {
        Ok(Some(result)) if result.confidence.is_match() => {
            let snr_db = voice_profile::estimate_snr_db(&samples_f32);

            // Incremental learning gdy very confident + dobre audio
            if result.score >= INCREMENTAL_LEARN_THRESHOLD {
                if let Err(e) = voice_profile::on_confident_match(
                    pool,
                    &result,
                    &embedding,
                    audio_duration_ms,
                    snr_db,
                    Some(meeting_id),
                ) {
                    warn!("Incremental learn failed: {}", e);
                }
            } else {
                // Nie jesto tak pewne — tylko touch last_seen
                if let Err(e) = crate::db::repository::touch_voice_profile(pool, result.profile_id) {
                    warn!("touch_voice_profile failed: {}", e);
                }
            }

            debug!(
                profile = %result.profile_name,
                score = result.score,
                confidence = ?result.confidence,
                "Matched to enrolled profile"
            );
            return Some(IdentifyResult {
                label: result.profile_name,
                profile_id: Some(result.profile_id),
                confidence: Some(result.score),
                is_enrolled: true,
                is_new_temp_speaker: false,
            });
        }
        Ok(_) => {
            // Brak confident match — przejdz do trackera meetingu
        }
        Err(e) => {
            warn!("match_to_profiles error: {}", e);
        }
    }

    // 2. Fallback: per-meeting tracker z DB persistence + auto-promotion
    let snr_db = voice_profile::estimate_snr_db(&samples_f32);

    let track_result = {
        let mut map = active_trackers().lock();
        let tracker = match map.get_mut(meeting_id) {
            Some(t) => t,
            None => {
                drop(map);
                if let Err(e) = start_meeting(pool, meeting_id) {
                    warn!("auto-start meeting failed: {}", e);
                    return None;
                }
                map = active_trackers().lock();
                map.get_mut(meeting_id)?
            }
        };

        match tracker.track(pool, &embedding, audio_duration_ms, snr_db) {
            Ok(r) => r,
            Err(e) => {
                warn!("tracker.track failed: {}", e);
                return None;
            }
        }
    };

    // 3. Jesli doszlo do auto-promocji podczas track() — ten sam embedding
    //    teraz istnieje jako voice_profile. Re-matchujemy zeby zwrocic nowa
    //    etykiete (KNOWN_SPEAKER_XX) juz dla BIEZACEJ wypowiedzi, zamiast
    //    czekac do nastepnego meetingu.
    if track_result.promoted {
        match voice_profile::match_to_profiles(pool, &embedding) {
            Ok(Some(promoted_match)) => {
                info!(
                    previous_label = %track_result.label,
                    new_label = %promoted_match.profile_name,
                    profile_id = promoted_match.profile_id,
                    score = promoted_match.score,
                    "Re-matched after auto-promotion"
                );
                return Some(IdentifyResult {
                    label: promoted_match.profile_name,
                    profile_id: Some(promoted_match.profile_id),
                    confidence: Some(promoted_match.score),
                    is_enrolled: true,
                    is_new_temp_speaker: false,
                });
            }
            Ok(None) => {
                warn!("Promotion succeeded but re-match returned no profile — using old label");
            }
            Err(e) => {
                warn!("Re-match after promotion failed: {}", e);
            }
        }
    }

    Some(IdentifyResult {
        label: track_result.label,
        profile_id: None,
        confidence: Some(track_result.similarity),
        is_enrolled: false,
        is_new_temp_speaker: track_result.is_new_speaker,
    })
}

/// Enrolment z raw PCM i16 LE. Dzieli audio na slidingowe okna, wylicza
/// embeddingi WeSpeakera, SNR per okno, buduje profil dla osoby.
pub fn enroll_profile_from_pcm(
    pool: &DbPool,
    identity: &PersonIdentity<'_>,
    pcm_i16_le: &[u8],
    source: &str,
) -> Result<EnrollmentResult, String> {
    let ext = extractor().ok_or_else(|| "WeSpeaker model nie zaladowany".to_string())?;

    let samples_f32 = voice_profile::pcm_i16_le_to_f32(pcm_i16_le);
    let total_duration_ms = (samples_f32.len() * 1000 / 16000) as u64;

    if samples_f32.len() < ENROLL_WINDOW_SAMPLES {
        return Err(format!(
            "audio za krotkie: {} probek ({:.2}s), wymagane minimum {:.1}s",
            samples_f32.len(),
            total_duration_ms as f32 / 1000.0,
            ENROLL_WINDOW_SAMPLES as f32 / 16000.0
        ));
    }

    // Sliding window 3s hop 0.75s → extract embeddings + SNR per window
    let mut enrollment_samples: Vec<EnrollmentSample> = Vec::new();
    let mut pos = 0;
    while pos + ENROLL_WINDOW_SAMPLES <= samples_f32.len() {
        let window = &samples_f32[pos..pos + ENROLL_WINDOW_SAMPLES];
        let embedding = match ext.extract(window) {
            Ok(e) => e,
            Err(e) => {
                warn!(window_pos = pos, "extract error: {}", e);
                pos += ENROLL_HOP_SAMPLES;
                continue;
            }
        };
        let snr = voice_profile::estimate_snr_db(window);
        let rms: f32 =
            (window.iter().map(|x| x * x).sum::<f32>() / window.len() as f32).sqrt();
        let window_duration_ms = (ENROLL_WINDOW_SAMPLES * 1000 / 16000) as u64;
        enrollment_samples.push(EnrollmentSample {
            embedding,
            duration_ms: window_duration_ms,
            snr_db: snr,
            rms,
        });
        pos += ENROLL_HOP_SAMPLES;
    }

    if enrollment_samples.is_empty() {
        return Err("nie udalo sie wyciagnac zadnego embeddingu".to_string());
    }

    voice_profile::enroll_profile(pool, identity, &enrollment_samples, source)
        .map_err(|e| format!("enrollment failed: {}", e))
}

/// Dopisuje sample do istniejacego profilu (incremental z PCM).
pub fn append_to_profile_from_pcm(
    pool: &DbPool,
    profile_id: i64,
    pcm_i16_le: &[u8],
    meeting_id: Option<&str>,
) -> Result<usize, String> {
    let ext = extractor().ok_or_else(|| "WeSpeaker model nie zaladowany".to_string())?;

    let samples_f32 = voice_profile::pcm_i16_le_to_f32(pcm_i16_le);
    if samples_f32.len() < ENROLL_WINDOW_SAMPLES {
        return Err("audio za krotkie do wzbogacenia profilu".to_string());
    }

    let mut added = 0;
    let mut pos = 0;
    while pos + ENROLL_WINDOW_SAMPLES <= samples_f32.len() {
        let window = &samples_f32[pos..pos + ENROLL_WINDOW_SAMPLES];
        if let Ok(embedding) = ext.extract(window) {
            let snr = voice_profile::estimate_snr_db(window);
            let duration_ms = (ENROLL_WINDOW_SAMPLES * 1000 / 16000) as u64;
            if let Ok(()) = voice_profile::add_sample_to_profile(
                pool,
                profile_id,
                &embedding,
                duration_ms,
                snr,
                meeting_id,
                "append",
            ) {
                added += 1;
            }
        }
        pos += ENROLL_HOP_SAMPLES;
    }

    Ok(added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use rusqlite::Connection;
    use std::sync::{Arc, Mutex as StdMutex};

    fn test_pool() -> DbPool {
        let conn = Connection::open_in_memory().unwrap();
        migrations::run(&conn).unwrap();
        Arc::new(StdMutex::new(conn))
    }

    #[test]
    fn start_and_end_meeting_lifecycle() {
        let pool = test_pool();
        start_meeting(&pool, "meet-test-1").unwrap();

        // Tracker is active
        {
            let map = active_trackers().lock();
            assert!(map.contains_key("meet-test-1"));
        }

        // End meeting — powinien byc wyczyszczony
        end_meeting(&pool, "meet-test-1").unwrap();
        {
            let map = active_trackers().lock();
            assert!(!map.contains_key("meet-test-1"));
        }
    }

    #[test]
    fn end_meeting_without_start_is_noop() {
        let pool = test_pool();
        end_meeting(&pool, "never-started").unwrap();
    }

    /// Integration test — pelny live flow:
    ///   1. start_meeting (symuluje JoinMeeting)
    ///   2. enroll 2 profile z audio
    ///   3. identify_speaker_with_profiles dla audio Jana → powinien zwrocic
    ///      imie Jana + profile_id, is_enrolled=true
    ///   4. identify dla audio Anny → powinien zwrocic Anne
    ///   5. identify dla nieznanego glosu → powinien fallbackowac na SPEAKER_XX
    ///   6. end_meeting (symuluje LeaveMeeting)
    ///   7. Sprawdz ze temp speakers sa w DB dla tego meetingu
    ///
    /// Uruchom:
    ///   DIARIZATION_MODEL_PATH=../models/diarization/embedding.onnx \
    ///     cargo test --lib --features inference-diarization \
    ///     full_live_flow_integration -- --nocapture --ignored
    #[test]
    #[ignore]
    fn full_live_flow_integration() {
        let pool = test_pool();
        let meeting_id = "test-meet-live-001";

        // 1. Start meeting
        start_meeting(&pool, meeting_id).unwrap();

        // 2. Enroll dwoch mowcow z prawdziwego audio
        let samples = read_wav_i16_16k("/tmp/sample_voices.wav")
            .expect("wymaga /tmp/sample_voices.wav");
        let glos1_i16 = &samples[0..16000 * 9 / 2];
        let glos2_i16 = &samples[5 * 16000..];
        let pcm1: Vec<u8> = glos1_i16.iter().flat_map(|&s| s.to_le_bytes()).collect();
        let pcm2: Vec<u8> = glos2_i16.iter().flat_map(|&s| s.to_le_bytes()).collect();

        let jan = PersonIdentity::new("Jan").with_last_name("Kowalski");
        let result1 = enroll_profile_from_pcm(&pool, &jan, &pcm1, "integration-test")
            .expect("Jan enroll");
        assert_eq!(result1.name, "Jan Kowalski");

        let anna = PersonIdentity::new("Anna").with_last_name("Nowak");
        let result2 = enroll_profile_from_pcm(&pool, &anna, &pcm2, "integration-test")
            .expect("Anna enroll");
        assert_eq!(result2.name, "Anna Nowak");

        // 3. Identify Jana → enrolled match
        let ident1 = identify_speaker_with_profiles(&pool, &pcm1, meeting_id)
            .expect("ident jan");
        assert_eq!(ident1.label, "Jan Kowalski");
        assert_eq!(ident1.profile_id, Some(result1.profile_id));
        assert!(ident1.is_enrolled);
        assert!(!ident1.is_new_temp_speaker);
        assert!(ident1.confidence.unwrap() > 0.55);
        println!(
            "Jan identified with confidence {:.3}",
            ident1.confidence.unwrap()
        );

        // 4. Identify Anny → enrolled match
        let ident2 = identify_speaker_with_profiles(&pool, &pcm2, meeting_id)
            .expect("ident anna");
        assert_eq!(ident2.label, "Anna Nowak");
        assert_eq!(ident2.profile_id, Some(result2.profile_id));
        assert!(ident2.is_enrolled);
        assert!(ident2.confidence.unwrap() > 0.55);
        println!(
            "Anna identified with confidence {:.3}",
            ident2.confidence.unwrap()
        );

        // 5. Symuluj nieznany glos — usuwamy oba profile, bierzemy Jana znow,
        //    powinien zostac fallback SPEAKER_XX
        crate::db::repository::delete_voice_profile(&pool, result1.profile_id).unwrap();
        crate::db::repository::delete_voice_profile(&pool, result2.profile_id).unwrap();

        let ident3 = identify_speaker_with_profiles(&pool, &pcm1, meeting_id)
            .expect("ident fallback");
        assert!(!ident3.is_enrolled, "no enrolled profiles left");
        assert!(ident3.label.starts_with("SPEAKER_"));
        println!("Unknown voice → {} (fallback)", ident3.label);

        // 6. End meeting — flush do DB
        end_meeting(&pool, meeting_id).unwrap();

        // 7. Sprawdz ze temp speaker jest w DB
        let temps = crate::db::repository::list_voice_temp_speakers(&pool, meeting_id).unwrap();
        assert!(!temps.is_empty(), "temp speakers should be persisted");
        println!(
            "DB temp speakers for {}: {:?}",
            meeting_id,
            temps.iter().map(|t| (&t.temp_label, t.sample_count)).collect::<Vec<_>>()
        );

        println!("=== Full live flow OK ===");
    }

    fn read_wav_i16_16k(path: &str) -> Result<Vec<i16>, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err("Not a WAV".into());
        }
        let mut pos = 12;
        while pos + 8 <= bytes.len() {
            let cid = &bytes[pos..pos + 4];
            let csz = u32::from_le_bytes([
                bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7],
            ]) as usize;
            pos += 8;
            if cid == b"data" {
                let pcm = &bytes[pos..pos + csz];
                return Ok(pcm.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect());
            }
            pos += csz;
        }
        Err("no data chunk".into())
    }
}
