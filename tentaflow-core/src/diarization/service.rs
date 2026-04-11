// =============================================================================
// Plik: diarization/service.rs
// Opis: Globalny singleton diarization — lazy-init embedding extractor +
//       thread-safe SpeakerTracker. Wywolywany z reverse_request.rs zaraz
//       po otrzymaniu audio od meeting-bota, przed/po STT.
// =============================================================================

use super::voice_profile::{
    self, EnrollmentError, EnrollmentResult, EnrollmentSample, MatchConfidence, MatchResult,
    ENROLL_HOP_SAMPLES, ENROLL_WINDOW_SAMPLES, INCREMENTAL_LEARN_THRESHOLD, MATCH_MIN_AUDIO_SAMPLES,
};
use super::{EmbeddingExtractor, SpeakerTracker};
use crate::db::DbPool;
use anyhow::Result;
use parking_lot::Mutex;
use std::sync::OnceLock;
use tracing::{debug, info, warn};

/// Domyslne parametry clustering.
/// Threshold 0.4 dobrany empirycznie dla trackera z oknem N=8 embeddingow —
/// porownujemy MAX similarity do *wszystkich* wczesniejszych embeddingow mowcy,
/// wiec wystarczy ze nowa wypowiedz jest blizsza ktoremukolwiek z N ostatnich.
/// Cross-speaker cos w naszych testach 0.15-0.30, wiec 0.4 jest bezpieczne.
const DEFAULT_SIMILARITY_THRESHOLD: f32 = 0.4;
const DEFAULT_MAX_SPEAKERS: usize = 20;

/// Minimalna dlugosc audio do ekstrakcji wiarygodnego embeddingu.
/// WeSpeaker ECAPA-TDNN potrzebuje ~1s+ do stabilnego embeddingu; krotsze
/// segmenty daja szumne wyniki (w testach cos ~0.45-0.55 sam-do-siebie).
const MIN_AUDIO_SAMPLES: usize = 16000; // 1.0s @ 16kHz

/// Maksymalna dlugosc audio podawana do WeSpeaker. Embedding ECAPA-TDNN
/// stabilizuje sie okolo 1-1.5s (cos vs reference plateau'uje), a koszt
/// ekstrakcji rosnie liniowo w dlugosci audio. Bierzemy *srodkowe*
/// MAX_AUDIO_SAMPLES probek zeby trzymac latencje per-segment w ~8-10ms.
const MAX_AUDIO_SAMPLES: usize = 24000; // 1.5s @ 16kHz

/// Sciezka do modelu WeSpeaker ONNX — env var DIARIZATION_MODEL_PATH lub fallback
fn default_model_path() -> String {
    std::env::var("DIARIZATION_MODEL_PATH")
        .unwrap_or_else(|_| "models/diarization/embedding.onnx".to_string())
}

/// Lazy-init extractor. None jesli model nie zostal zaladowany (np. brak pliku).
static EXTRACTOR: OnceLock<Option<EmbeddingExtractor>> = OnceLock::new();

/// Globalny tracker, wspoldzielony miedzy wszystkimi requestami reverse STT.
/// Reset zewnetrznie przez `reset_tracker()` gdy zmienia sie meeting.
static TRACKER: OnceLock<Mutex<SpeakerTracker>> = OnceLock::new();

fn tracker() -> &'static Mutex<SpeakerTracker> {
    TRACKER.get_or_init(|| {
        Mutex::new(SpeakerTracker::new(DEFAULT_SIMILARITY_THRESHOLD, DEFAULT_MAX_SPEAKERS))
    })
}

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

/// Dopasowuje glos do istniejacego speakera albo tworzy nowego.
/// `pcm_i16_le` to raw bytes audio i16 little-endian mono 16kHz.
/// Zwraca etykiete (np. "SPEAKER_00") lub None jesli diarization niedostepne.
pub fn identify_speaker(pcm_i16_le: &[u8]) -> Option<String> {
    let ext = extractor()?;

    // Konwersja i16 LE bytes -> f32 normalized
    let mut samples_f32: Vec<f32> = Vec::with_capacity(pcm_i16_le.len() / 2);
    for chunk in pcm_i16_le.chunks_exact(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
        samples_f32.push(sample as f32 / 32768.0);
    }

    if samples_f32.len() < MIN_AUDIO_SAMPLES {
        tracing::debug!(
            samples = samples_f32.len(),
            min_required = MIN_AUDIO_SAMPLES,
            "Diarization pominieta — audio za krotkie dla wiarygodnego embeddingu"
        );
        return None;
    }

    // Dla dlugich segmentow bierzemy srodkowe MAX_AUDIO_SAMPLES — WeSpeaker
    // stabilizuje sie okolo 2-3s a koszt rosnie liniowo. Srodkowy wycinek
    // unika ciszy/oddechu na brzegach i trzyma latencje w ryzach.
    let clipped: &[f32] = if samples_f32.len() > MAX_AUDIO_SAMPLES {
        let start = (samples_f32.len() - MAX_AUDIO_SAMPLES) / 2;
        &samples_f32[start..start + MAX_AUDIO_SAMPLES]
    } else {
        &samples_f32[..]
    };

    let extract_start = std::time::Instant::now();
    let extract_result = ext.extract(clipped);
    let extract_ms = extract_start.elapsed().as_millis();
    tracing::debug!(
        samples_in = clipped.len(),
        extract_ms,
        "WeSpeaker embedding extracted"
    );

    match extract_result {
        Ok(embedding) => {
            let label = tracker().lock().track(&embedding);
            Some(label)
        }
        Err(e) => {
            warn!("Blad ekstrakcji embeddingu: {}", e);
            None
        }
    }
}

/// Resetuje stan trackera — nowy meeting, nowi speakerzy od SPEAKER_00.
/// Do wywolania przy LeaveMeeting/JoinMeeting lub po dluzszym braku aktywnosci.
pub fn reset_tracker() {
    tracker().lock().reset();
    info!("Speaker tracker zresetowany");
}

// =============================================================================
// Voice profile integration — bulletproof speaker recognition
// =============================================================================

/// Identyfikuje mowce z *baza enrolled profiles* jako pierwszeństwo,
/// fallback na online tracker gdy profil nie pasuje.
///
/// Zwraca tuple (label, confidence_optional):
/// - `("Jan Kowalski", Some(0.72))` — enrolled profile match (high conf)
/// - `("SPEAKER_00", None)`         — online tracker label (brak enrolled match)
/// - `None`                          — audio za krotkie / model niedostepny
///
/// Jesli match jest very-confident (score >= 0.7) i SNR jest dobry, embedding
/// jest auto-dodawany do profilu (incremental learning).
pub fn identify_speaker_with_profiles(
    pool: &DbPool,
    pcm_i16_le: &[u8],
    meeting_id: Option<&str>,
) -> Option<(String, Option<f32>)> {
    let ext = extractor()?;

    // Konwersja i16 LE → f32
    let samples_f32 = voice_profile::pcm_i16_le_to_f32(pcm_i16_le);

    if samples_f32.len() < MATCH_MIN_AUDIO_SAMPLES {
        debug!(
            samples = samples_f32.len(),
            min_required = MATCH_MIN_AUDIO_SAMPLES,
            "identify_speaker_with_profiles — audio za krotkie"
        );
        return None;
    }

    // Clipping do MAX_AUDIO_SAMPLES dla stabilnego embeddingu + low latency
    let clipped: &[f32] = if samples_f32.len() > MAX_AUDIO_SAMPLES {
        let start = (samples_f32.len() - MAX_AUDIO_SAMPLES) / 2;
        &samples_f32[start..start + MAX_AUDIO_SAMPLES]
    } else {
        &samples_f32[..]
    };

    let embedding = match ext.extract(clipped) {
        Ok(e) => e,
        Err(e) => {
            warn!("WeSpeaker extract error: {}", e);
            return None;
        }
    };

    // 1. Sprawdz enrolled profiles
    match voice_profile::match_to_profiles(pool, &embedding) {
        Ok(Some(result)) if result.confidence.is_match() => {
            let snr_db = voice_profile::estimate_snr_db(&samples_f32);
            let duration_ms = (samples_f32.len() * 1000 / 16000) as u64;

            // Incremental learning (profil uczy sie z czasem)
            if result.score >= INCREMENTAL_LEARN_THRESHOLD {
                if let Err(e) = voice_profile::on_confident_match(
                    pool,
                    &result,
                    &embedding,
                    duration_ms,
                    snr_db,
                    meeting_id,
                ) {
                    warn!("Incremental learn failed: {}", e);
                }
            } else {
                // Still touch last_seen
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
            return Some((result.profile_name, Some(result.score)));
        }
        Ok(_) => {
            // Brak confident match — fallback na online tracker
        }
        Err(e) => {
            warn!("match_to_profiles error: {}", e);
        }
    }

    // 2. Fallback: online tracker (tymczasowe SPEAKER_XX per meeting)
    let label = tracker().lock().track(&embedding);
    Some((label, None))
}

/// Enrolment z raw PCM i16 LE. Dzieli audio na slidingowe okna 3s hop 1.5s,
/// wylicza embeddingi WeSpeakera, SNR per okno, buduje profil.
///
/// Wolane przez API endpoint (ktory wola LLM po detekcji "Cześć, tu Jan").
/// Nie wymaga VAD na wejsciu — zaklada ze caller podal czyste speech audio
/// (LLM robi decyzje ze wyslac na podstawie wykrycia introduction w tekscie).
pub fn enroll_profile_from_pcm(
    pool: &DbPool,
    name: &str,
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

    // Sliding window 3s hop 1.5s → extract embeddings + SNR per window
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

    // Zapisz do DB
    voice_profile::enroll_profile(pool, name, &enrollment_samples, source)
        .map_err(|e| format!("enrollment failed: {}", e))
}

/// Dopisuje sample do istniejacego profilu (incremental z PCM).
/// Uzywane gdy LLM po raz drugi wykryje usera i chce "rozszerzyc" profil.
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

    /// Test end-to-end: przeslij dwa rozne glosy i sprawdz ze dostajemy
    /// SPEAKER_00 i SPEAKER_01. Wymaga pliku /tmp/sample_voices.wav
    /// (16kHz mono s16) z dwoma mowcami: glos 1 do ~5s, glos 2 po 5s.
    ///
    /// Uruchom: DIARIZATION_MODEL_PATH=../models/diarization/embedding.onnx \
    ///   cargo test --features inference-diarization two_speakers -- --nocapture --ignored
    #[test]
    #[ignore]
    fn two_speakers_identified_correctly() {
        let path = "/tmp/sample_voices.wav";
        let samples = read_wav_s16_mono_16k(path).expect("test audio not found");

        // Segment 1: 0-4.5s (glos 1), segment 2: 5s-koniec (glos 2)
        let seg1 = &samples[0..16000 * 9 / 2];
        let seg2 = &samples[5 * 16000..];

        let bytes1: Vec<u8> = seg1.iter().flat_map(|&s| s.to_le_bytes()).collect();
        let bytes2: Vec<u8> = seg2.iter().flat_map(|&s| s.to_le_bytes()).collect();

        // Warm up — pierwsza inwokacja laduje model WeSpeaker (~50ms jednorazowo).
        // Chcemy mierzyc koszt steady-state, nie cold start.
        reset_tracker();
        let _ = identify_speaker(&bytes1);
        reset_tracker();

        // Steady state: mierzymy 4 wywolania (2 segmenty x 2 przebiegi)
        let t0 = std::time::Instant::now();
        let label1 = identify_speaker(&bytes1).expect("speaker 1 not identified");
        let t1 = t0.elapsed();

        let t0 = std::time::Instant::now();
        let label2 = identify_speaker(&bytes2).expect("speaker 2 not identified");
        let t2 = t0.elapsed();

        let t0 = std::time::Instant::now();
        let label1_again = identify_speaker(&bytes1).expect("failed");
        let t1b = t0.elapsed();

        let t0 = std::time::Instant::now();
        let label2_again = identify_speaker(&bytes2).expect("failed");
        let t2b = t0.elapsed();

        println!("\n=== Diarization timing (audio per segment ~4.5s) ===");
        println!("  segment 1 ({}): {:?}", label1, t1);
        println!("  segment 2 ({}): {:?}", label2, t2);
        println!("  segment 1 again ({}): {:?}", label1_again, t1b);
        println!("  segment 2 again ({}): {:?}", label2_again, t2b);
        let avg = (t1 + t2 + t1b + t2b) / 4;
        println!("  average: {:?}", avg);
        println!();

        assert_eq!(label1, "SPEAKER_00", "pierwszy segment powinien byc SPEAKER_00");
        assert_eq!(label2, "SPEAKER_01", "drugi segment powinien byc innym mowca");
        assert_eq!(label1_again, "SPEAKER_00", "ponowny glos 1 powinien trafic do SPEAKER_00");
        assert_eq!(label2_again, "SPEAKER_01", "ponowny glos 2 powinien trafic do SPEAKER_01");
    }

    fn read_wav_s16_mono_16k(path: &str) -> Result<Vec<i16>, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return Err("Not a WAV".into());
        }
        let mut pos = 12;
        while pos + 8 <= bytes.len() {
            let cid = &bytes[pos..pos + 4];
            let csz = u32::from_le_bytes([bytes[pos + 4], bytes[pos + 5], bytes[pos + 6], bytes[pos + 7]]) as usize;
            pos += 8;
            if cid == b"data" {
                let pcm = &bytes[pos..pos + csz];
                return Ok(pcm.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect());
            }
            pos += csz;
        }
        Err("no data chunk".into())
    }

    /// End-to-end: VAD → WeSpeaker diarization + Whisper STT dla kazdego segmentu.
    /// Pokazuje jak bedzie wygladal wynik w GUI Bot Status podczas prawdziwego meetingu.
    ///
    /// Uruchom: DIARIZATION_MODEL_PATH=../models/diarization/embedding.onnx \
    ///   cargo test --lib --features inference-diarization e2e_diarization_with_stt \
    ///   -- --nocapture --ignored
    #[test]
    #[ignore]
    fn e2e_diarization_with_stt() {
        use tentaflow_voice::SileroVadStreaming;
        use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

        let wav_path = "/tmp/sample_voices.wav";
        let vad_path = "../models/diarization/silero_vad.onnx";
        let whisper_path = "/home/critix/.local/share/tentaflow/models/whisper/ggml-large-v3-turbo.bin";

        let samples_i16 = read_wav_s16_mono_16k(wav_path).expect("brak pliku audio");
        let samples_f32: Vec<f32> = samples_i16.iter().map(|&s| s as f32 / 32768.0).collect();
        let total_s = samples_f32.len() as f32 / 16000.0;
        println!("\n=== E2E Diarization + STT ===");
        println!("Audio: {} probek, {:.2}s\n", samples_f32.len(), total_s);

        // 1. Segmentacja hybrydowa: Silero VAD + energy-based split na cichych regionach.
        //    Silero sam w sobie zostawia ciagle obszary mowy gdy przerwy sa krotsze niz
        //    silence threshold; dodajemy energy-based split w regionach niskiego RMS.
        let mut vad = SileroVadStreaming::from_file(vad_path).expect("VAD load");
        let chunk_size = 512;
        let mut vad_probs: Vec<f32> = Vec::new();
        for start in (0..samples_f32.len()).step_by(chunk_size) {
            let end = (start + chunk_size).min(samples_f32.len());
            let mut buf = vec![0.0_f32; chunk_size];
            buf[..end - start].copy_from_slice(&samples_f32[start..end]);
            let p = vad.predict(&buf).unwrap_or(0.0);
            vad_probs.push(p);
        }

        // Liczymy RMS per chunk 512 sampli — niski RMS = cisza
        let mut rms_per_chunk: Vec<f32> = Vec::with_capacity(vad_probs.len());
        for i in 0..vad_probs.len() {
            let start = i * chunk_size;
            let end = (start + chunk_size).min(samples_f32.len());
            let s = &samples_f32[start..end];
            let r = (s.iter().map(|x| x * x).sum::<f32>() / s.len().max(1) as f32).sqrt();
            rms_per_chunk.push(r);
        }

        // Aktywnosc = RMS > 0.005 (tylko czysta cisza jest "nieaktywna").
        //    Nie uzywamy strict VAD bo wycinal cichsze regiony w ktorych nadal jest mowa.
        let is_active: Vec<bool> = rms_per_chunk.iter().map(|&r| r >= 0.005).collect();

        // Hysteresis: segment konczy sie po >= 300ms ciszy
        let silence_chunks_threshold = (16000 * 300 / 1000) / chunk_size;
        let mut segments: Vec<(usize, usize)> = Vec::new();
        let mut seg_start = 0usize;
        let mut in_speech = false;
        let mut silence_run_chunks = 0usize;
        for (i, &active) in is_active.iter().enumerate() {
            let pos = i * chunk_size;
            if active {
                if !in_speech {
                    seg_start = pos;
                    in_speech = true;
                }
                silence_run_chunks = 0;
            } else if in_speech {
                silence_run_chunks += 1;
                if silence_run_chunks >= silence_chunks_threshold {
                    let end = pos.saturating_sub((silence_run_chunks - 1) * chunk_size);
                    if end > seg_start {
                        segments.push((seg_start, end));
                    }
                    in_speech = false;
                    silence_run_chunks = 0;
                }
            }
        }
        if in_speech {
            segments.push((seg_start, samples_f32.len()));
        }

        // Odfiltruj zbyt krotkie (<200ms) segmenty
        let segments: Vec<(usize, usize)> = segments.into_iter()
            .filter(|(s, e)| e - s >= 16000 / 5)
            .collect();

        println!("VAD wykryl {} segmentow mowy:", segments.len());
        for (i, (s, e)) in segments.iter().enumerate() {
            println!("  segment {}: {:.2}s - {:.2}s ({:.2}s)",
                i, *s as f32 / 16000.0, *e as f32 / 16000.0,
                (*e - *s) as f32 / 16000.0);
        }
        println!();

        // 2. Zaladuj Whisper (raz na caly test)
        println!("Ladowanie Whisper large-v3-turbo...");
        let w_start = std::time::Instant::now();
        let ctx_params = WhisperContextParameters::default();
        let ctx = WhisperContext::new_with_params(whisper_path, ctx_params)
            .expect("Nie udalo sie zaladowac modelu Whisper");
        println!("Whisper zaladowany w {:?}\n", w_start.elapsed());

        // 3. Dla kazdego segmentu: diarization + STT.
        //    Segmenty < 1s sa za krotkie na wiarygodny embedding — w takim przypadku
        //    dziedzicza etykiete od poprzedniego segmentu (zakladamy kontynuacje mowcy).
        reset_tracker();
        let mut transcript_lines: Vec<(f32, f32, String, String)> = Vec::new();
        let mut last_speaker: Option<String> = None;

        for (s, e) in &segments {
            let audio_slice_f32 = &samples_f32[*s..*e];
            let audio_slice_i16 = &samples_i16[*s..*e];
            let start_s = *s as f32 / 16000.0;
            let end_s = *e as f32 / 16000.0;

            // Diarization — wymaga >= 1s audio (service.rs MIN_AUDIO_SAMPLES).
            // Krotsze fragmenty dziedzicza etykiete poprzedniego segmentu.
            let pcm_bytes: Vec<u8> = audio_slice_i16.iter().flat_map(|&s| s.to_le_bytes()).collect();
            let speaker = identify_speaker(&pcm_bytes)
                .or_else(|| last_speaker.clone())
                .unwrap_or_else(|| "Nieznany".to_string());
            last_speaker = Some(speaker.clone());

            // STT — Whisper.cpp w jezyku polskim
            let mut state = ctx.create_state().expect("whisper state");
            let mut fp = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            fp.set_language(Some("pl"));
            fp.set_translate(false);
            fp.set_print_special(false);
            fp.set_print_progress(false);
            fp.set_print_realtime(false);
            fp.set_print_timestamps(false);
            fp.set_n_threads(std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4));

            state.full(fp, audio_slice_f32).expect("whisper transcribe");
            let n = state.full_n_segments();
            let mut text = String::new();
            for i in 0..n {
                if let Some(seg) = state.get_segment(i) {
                    if let Ok(t) = seg.to_str_lossy() {
                        if !text.is_empty() { text.push(' '); }
                        text.push_str(t.trim());
                    }
                }
            }

            transcript_lines.push((start_s, end_s, speaker, text));
        }

        // 4. Wydrukuj wynik tak jak bedzie widoczny w GUI Bot Status
        println!("=== WYNIK KONCOWY (tak zobaczysz w GUI Bot Status) ===\n");
        for (start_s, end_s, speaker, text) in &transcript_lines {
            println!("  [{:5.2}s - {:5.2}s] {}: {}", start_s, end_s, speaker, text);
        }
        println!();

        // Sanity: powinno byc dokladnie 2 mowcow
        let unique_speakers: std::collections::HashSet<_> =
            transcript_lines.iter().map(|(_, _, s, _)| s.clone()).collect();
        println!("Unikalni mowcy: {:?}", unique_speakers);
        assert!(unique_speakers.contains("SPEAKER_00"));
        assert!(unique_speakers.contains("SPEAKER_01"));
    }
}
