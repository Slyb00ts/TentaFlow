// =============================================================================
// Plik: diarization/voice_profile.rs
// Opis: Bulletproof speaker recognition — enrollment, matching, incremental
//       learning. Model: WeSpeaker ECAPA-TDNN (pure Rust, cos=1.0 vs ORT).
//
//       Strategia:
//       - Enrollment: 10-60s audio → VAD → sliding 3s windows → wiele samples
//         → weryfikacja spojnosci (intra-similarity) → zapis do DB
//       - Matching: nowy embedding vs WSZYSTKIE samples profilu (top-K mean)
//         + centroid — kombinacja daje odpornosc na wariancje akustyczna
//       - Incremental learning: wysoka pewnosc + dobry SNR → auto-dodaj sample
//
//       Thresholdy dostrojone empirycznie dla WeSpeaker 192-dim embeddings:
//         >= 0.60 very confident
//         >= 0.45 confident      (same-speaker naturalna mowa)
//         >= 0.30 uncertain
//         <  0.30 no match       (cross-speaker WeSpeaker typowo 0.05-0.20)
// =============================================================================

use crate::db::models::{NewVoiceProfile, NewVoiceProfileSample};
use crate::db::{repository as repo, DbPool};
use anyhow::{bail, Context, Result};
use tracing::{debug, info, warn};

const EMBEDDING_DIM: usize = 192;

/// Progi matchingu (konsekwentne z Pyannote/speechbrain best practices)
pub const MATCH_VERY_CONFIDENT: f32 = 0.70;
pub const MATCH_CONFIDENT: f32 = 0.55;
pub const MATCH_UNCERTAIN: f32 = 0.45;

/// Minimalne wymagania enrollment dla wiarygodnego profilu.
/// 4s minimum = 3 overlapping 3s windows (span 4.5s). Dla lepszego profilu
/// LLM powinien wysylac 10-30s audio → ~10-40 samples.
pub const MIN_ENROLLMENT_DURATION_MS: u64 = 4_000;
pub const MIN_ENROLLMENT_SAMPLES: usize = 3;
pub const MAX_ENROLLMENT_SAMPLES: usize = 20;
/// Minimalna wewnetrzna spojnosc (srednia cos miedzy samples) do akceptacji profilu.
/// Empirycznie: WeSpeaker same-speaker inter-utterance srednio 0.40-0.55 dla
/// naturalnej mowy, niektorzy mowcy spadaja do 0.30-0.40. Caller powinien
/// wyfiltrowac outliery przed wywolaniem enroll_profile (top-K by centroid).
pub const MIN_INTRA_SIMILARITY: f32 = 0.30;

/// Parametry slidingu dla enrollment — 3s okno, 0.75s hop daje 4 samples na 5s
/// audio i ~40 samples na 30s audio.
pub const ENROLL_WINDOW_SAMPLES: usize = 48000; // 3s @ 16kHz
pub const ENROLL_HOP_SAMPLES: usize = 12000; // 0.75s hop

/// Minimalna dlugosc audio dla matchingu (weryfikacja vs enrolled)
pub const MATCH_MIN_AUDIO_SAMPLES: usize = 16000; // 1.0s
/// Incremental learning dodaje sample tylko gdy match_score >= tego progu.
/// Musi byc wyzszy niz MATCH_CONFIDENT (0.55), zeby nie zanieczyszczac profilu
/// wpisami ktore tylko ledwo kwalifikuja sie jako match — false-positivy
/// zlepialyby profile roznych mowcow.
pub const INCREMENTAL_LEARN_THRESHOLD: f32 = 0.65;
/// I wymaga minimalnego SNR
pub const INCREMENTAL_MIN_SNR: f32 = 15.0;
/// Max samples per profil — po osiagnieciu najstarsze sa pomijane w incremental
pub const MAX_SAMPLES_PER_PROFILE: i64 = 50;

/// Wynik matchingu nowego embeddingu z wszystkimi enrolled profiles
#[derive(Debug, Clone)]
pub struct MatchResult {
    pub profile_id: i64,
    pub profile_name: String,
    pub score: f32,
    pub confidence: MatchConfidence,
    /// Rozbicie scoringu — dla debugowania i incremental learning decisions
    pub centroid_similarity: f32,
    pub topk_mean: f32,
    pub max_similarity: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchConfidence {
    VeryConfident,  // >= 0.70
    Confident,      // >= 0.55
    Uncertain,      // >= 0.45 (NIE jest traktowany jako match)
    NoMatch,        // < 0.45
}

impl MatchConfidence {
    pub fn from_score(score: f32) -> Self {
        if score >= MATCH_VERY_CONFIDENT {
            Self::VeryConfident
        } else if score >= MATCH_CONFIDENT {
            Self::Confident
        } else if score >= MATCH_UNCERTAIN {
            Self::Uncertain
        } else {
            Self::NoMatch
        }
    }

    pub fn is_match(&self) -> bool {
        matches!(self, Self::VeryConfident | Self::Confident)
    }
}

/// Statystyka enrollment sample — embedding + metadata per okno
#[derive(Debug, Clone)]
pub struct EnrollmentSample {
    pub embedding: Vec<f32>,
    pub duration_ms: u64,
    pub snr_db: f32,
    pub rms: f32,
}

/// Dane identyfikacyjne osoby — przekazywane do enrollment.
#[derive(Debug, Clone)]
pub struct PersonIdentity<'a> {
    pub first_name: &'a str,
    pub last_name: Option<&'a str>,
    pub nickname: Option<&'a str>,
}

impl<'a> PersonIdentity<'a> {
    pub fn new(first_name: &'a str) -> Self {
        Self {
            first_name,
            last_name: None,
            nickname: None,
        }
    }

    pub fn with_last_name(mut self, last_name: &'a str) -> Self {
        self.last_name = Some(last_name);
        self
    }

    pub fn with_nickname(mut self, nickname: &'a str) -> Self {
        self.nickname = Some(nickname);
        self
    }

    /// Wylicza display name — unique identifier profilu.
    /// "Jan Kowalski (janek)" | "Jan Kowalski" | "Jan (janek)" | "Jan"
    pub fn display_name(&self) -> String {
        let first = self.first_name.trim();
        let last = self.last_name.map(str::trim).filter(|s| !s.is_empty());
        let nick = self.nickname.map(str::trim).filter(|s| !s.is_empty());
        match (last, nick) {
            (Some(l), Some(n)) => format!("{} {} ({})", first, l, n),
            (Some(l), None) => format!("{} {}", first, l),
            (None, Some(n)) => format!("{} ({})", first, n),
            (None, None) => first.to_string(),
        }
    }

    /// Walidacja: imie musi byc niepuste po trim
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.first_name.trim().is_empty() {
            return Err("first_name cannot be empty".to_string());
        }
        Ok(())
    }
}

/// Wynik enrollment — utworzony/zaktualizowany profil
#[derive(Debug, Clone)]
pub struct EnrollmentResult {
    pub profile_id: i64,
    pub name: String,
    pub samples_accepted: usize,
    pub samples_rejected: usize,
    pub reliability_score: f32,
    pub centroid: Vec<f32>,
}

/// Powód rejekcji enrollment
#[derive(Debug, Clone)]
pub enum EnrollmentError {
    TooShortAudio { got_ms: u64, required_ms: u64 },
    TooFewSamples { got: usize, required: usize },
    LowSnr { got_db: f32, required_db: f32 },
    InconsistentSamples { intra_similarity: f32, required: f32 },
    NoSpeech,
    ModelUnavailable,
}

impl std::fmt::Display for EnrollmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShortAudio { got_ms, required_ms } => write!(
                f,
                "audio za krotkie: {} ms, wymagane minimum {} ms",
                got_ms, required_ms
            ),
            Self::TooFewSamples { got, required } => write!(
                f,
                "za malo slidingowych probek: {}, wymagane minimum {}",
                got, required
            ),
            Self::LowSnr { got_db, required_db } => write!(
                f,
                "za duzo szumu: SNR {:.1} dB, wymagane >= {:.1} dB",
                got_db, required_db
            ),
            Self::InconsistentSamples { intra_similarity, required } => write!(
                f,
                "niespojne probki glosu: intra-similarity {:.3}, wymagane >= {:.3}",
                intra_similarity, required
            ),
            Self::NoSpeech => write!(f, "brak mowy w audio (po VAD)"),
            Self::ModelUnavailable => write!(f, "model WeSpeaker nie jest zaladowany"),
        }
    }
}

impl std::error::Error for EnrollmentError {}

/// Cosine similarity na znormalizowanych vectorach.
/// Zaklada ze embeddings sa typowo nieznormalizowane (raw WeSpeaker output).
#[inline]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0_f32;
    let mut na = 0.0_f32;
    let mut nb = 0.0_f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let norm = (na * nb).sqrt();
    if norm < 1e-12 {
        0.0
    } else {
        dot / norm
    }
}

/// L2-normalizacja w miejscu
pub fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        let inv = 1.0 / norm;
        for x in v.iter_mut() {
            *x *= inv;
        }
    }
}

/// Oblicza centroid (srednia znormalizowana) zestawu embeddingow
pub fn compute_centroid(samples: &[Vec<f32>]) -> Vec<f32> {
    if samples.is_empty() {
        return vec![0.0; EMBEDDING_DIM];
    }
    let dim = samples[0].len();
    let mut centroid = vec![0.0_f32; dim];
    for sample in samples {
        for i in 0..dim {
            centroid[i] += sample[i];
        }
    }
    let inv_n = 1.0 / samples.len() as f32;
    for v in centroid.iter_mut() {
        *v *= inv_n;
    }
    l2_normalize(&mut centroid);
    centroid
}

/// Wylicza srednia wewnetrzna cos similarity (intra-profile coherence)
pub fn intra_similarity(samples: &[Vec<f32>]) -> f32 {
    if samples.len() < 2 {
        return 1.0;
    }
    let mut sum = 0.0_f32;
    let mut count = 0;
    for i in 0..samples.len() {
        for j in (i + 1)..samples.len() {
            sum += cosine_similarity(&samples[i], &samples[j]);
            count += 1;
        }
    }
    if count == 0 {
        1.0
    } else {
        sum / count as f32
    }
}

/// Konwertuje embedding f32 → bajty little-endian do zapisu w BLOB
pub fn embedding_to_bytes(embedding: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(embedding.len() * 4);
    for v in embedding {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes
}

/// Konwertuje bajty little-endian → embedding f32
pub fn bytes_to_embedding(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Oblicza SNR (signal-to-noise ratio) w dB z audio PCM f32.
/// Heurystyka: sygnal = percentyl 90 energii ramki, szum = percentyl 10.
/// Zwraca wartosc w dB (im wieksze tym czystsze audio).
pub fn estimate_snr_db(samples: &[f32]) -> f32 {
    if samples.len() < 1600 {
        return 0.0;
    }
    // Dziel na ramki 100ms (1600 sampli @ 16kHz), licz RMS kazdej
    let frame_size = 1600;
    let n_frames = samples.len() / frame_size;
    if n_frames < 4 {
        return 0.0;
    }
    let mut rms_frames: Vec<f32> = (0..n_frames)
        .map(|i| {
            let frame = &samples[i * frame_size..(i + 1) * frame_size];
            let sum_sq: f32 = frame.iter().map(|x| x * x).sum();
            (sum_sq / frame_size as f32).sqrt()
        })
        .collect();
    rms_frames.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p10_idx = n_frames / 10;
    let p90_idx = (n_frames * 9) / 10;
    let noise = rms_frames[p10_idx].max(1e-6);
    let signal = rms_frames[p90_idx];
    20.0 * (signal / noise).log10()
}

/// Konwertuje i16 LE bytes -> f32 znormalizowane [-1, 1]
pub fn pcm_i16_le_to_f32(pcm: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(pcm.len() / 2);
    for chunk in pcm.chunks_exact(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]);
        out.push(s as f32 / 32768.0);
    }
    out
}

// =============================================================================
// Business logic — enrollment + matching (niezalezne od DB, czyste funkcje)
// =============================================================================

/// Waliduje i buduje profil z listy slidingowych sample embeddingow.
/// Nie dotyka DB — caller decyduje czy zapisac.
pub fn build_profile_from_samples(
    samples: &[EnrollmentSample],
) -> std::result::Result<(Vec<f32>, f32, Vec<usize>), EnrollmentError> {
    // Sprawdz minimum
    if samples.len() < MIN_ENROLLMENT_SAMPLES {
        return Err(EnrollmentError::TooFewSamples {
            got: samples.len(),
            required: MIN_ENROLLMENT_SAMPLES,
        });
    }

    // Unique audio span = (N-1) * hop + window_len. Samples overlapuja, wiec
    // suma duration_ms dalaby zawyzony coverage. Uzywamy samples[0] jako proxy
    // na window size.
    let window_ms = samples.first().map(|s| s.duration_ms).unwrap_or(0);
    let hop_ms = (ENROLL_HOP_SAMPLES * 1000 / 16000) as u64;
    let span_ms = window_ms + hop_ms * (samples.len() as u64 - 1);
    if span_ms < MIN_ENROLLMENT_DURATION_MS {
        return Err(EnrollmentError::TooShortAudio {
            got_ms: span_ms,
            required_ms: MIN_ENROLLMENT_DURATION_MS,
        });
    }

    // Filtruj samples z za niskim SNR
    let accepted_indices: Vec<usize> = samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.snr_db >= 10.0) // bardzo luzny filter tu, szczegolowy w caller
        .map(|(i, _)| i)
        .collect();

    if accepted_indices.len() < MIN_ENROLLMENT_SAMPLES {
        let avg_snr: f32 = if samples.is_empty() {
            0.0
        } else {
            samples.iter().map(|s| s.snr_db).sum::<f32>() / samples.len() as f32
        };
        return Err(EnrollmentError::LowSnr {
            got_db: avg_snr,
            required_db: 10.0,
        });
    }

    // Ogranicz do MAX_ENROLLMENT_SAMPLES — bierzemy N o najwyzszym SNR
    let mut idx_by_snr: Vec<usize> = accepted_indices.clone();
    idx_by_snr.sort_by(|a, b| {
        samples[*b]
            .snr_db
            .partial_cmp(&samples[*a].snr_db)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    idx_by_snr.truncate(MAX_ENROLLMENT_SAMPLES);

    let selected_embeddings: Vec<Vec<f32>> = idx_by_snr
        .iter()
        .map(|&i| samples[i].embedding.clone())
        .collect();

    // Weryfikacja spojnosci — czy to wszystko ten sam glos?
    let intra_sim = intra_similarity(&selected_embeddings);
    if intra_sim < MIN_INTRA_SIMILARITY {
        return Err(EnrollmentError::InconsistentSamples {
            intra_similarity: intra_sim,
            required: MIN_INTRA_SIMILARITY,
        });
    }

    // Liczymy centroid jako L2-normalized mean
    let centroid = compute_centroid(&selected_embeddings);

    Ok((centroid, intra_sim, idx_by_snr))
}

/// Enrollment z pre-policzonymi embeddingami — zapisuje do DB.
/// `samples` to embeddings + metadata z slidingu (caller uzywa
/// tentaflow_voice::WeSpeaker::extract).
pub fn enroll_profile(
    pool: &DbPool,
    identity: &PersonIdentity<'_>,
    samples: &[EnrollmentSample],
    source: &str,
) -> Result<EnrollmentResult> {
    identity
        .validate()
        .map_err(|e| anyhow::anyhow!("invalid identity: {}", e))?;

    let (centroid, reliability_score, selected_indices) = build_profile_from_samples(samples)
        .map_err(|e| anyhow::anyhow!("enrollment rejected: {}", e))?;

    let display_name = identity.display_name();

    // Sprawdz czy profil o tej nazwie juz istnieje
    if let Some(existing) = repo::get_voice_profile_by_name(pool, &display_name)? {
        bail!(
            "profil o nazwie '{}' juz istnieje (id={}), uzyj add_samples_to_profile",
            display_name,
            existing.id
        );
    }

    // Stworz profil
    let centroid_bytes = embedding_to_bytes(&centroid);
    let avg_snr: f32 = selected_indices
        .iter()
        .map(|&i| samples[i].snr_db)
        .sum::<f32>()
        / selected_indices.len() as f32;
    // Unique audio span (bez double-counting overlapping windows)
    let window_ms = samples.first().map(|s| s.duration_ms).unwrap_or(0);
    let hop_ms = (ENROLL_HOP_SAMPLES * 1000 / 16000) as u64;
    let span_ms = window_ms + hop_ms * (samples.len() as u64 - 1);
    let metadata = serde_json::json!({
        "initial_samples": selected_indices.len(),
        "avg_snr_db": avg_snr,
        "audio_span_ms": span_ms,
    })
    .to_string();

    let profile_id = repo::create_voice_profile(
        pool,
        &NewVoiceProfile {
            name: &display_name,
            first_name: identity.first_name.trim(),
            last_name: identity.last_name.map(str::trim).filter(|s| !s.is_empty()),
            nickname: identity.nickname.map(str::trim).filter(|s| !s.is_empty()),
            centroid: &centroid_bytes,
            sample_count: selected_indices.len() as i64,
            reliability_score,
            source,
            metadata_json: &metadata,
        },
    )?;

    // Zapisz samples (tylko zaakceptowane)
    for &idx in &selected_indices {
        let s = &samples[idx];
        let emb_bytes = embedding_to_bytes(&s.embedding);
        // intra_similarity dla pojedynczego sample = srednia jego cos z pozostalymi
        let mut per_sample_intra = 0.0_f32;
        let mut cnt = 0;
        for &other_idx in &selected_indices {
            if other_idx != idx {
                per_sample_intra +=
                    cosine_similarity(&samples[idx].embedding, &samples[other_idx].embedding);
                cnt += 1;
            }
        }
        let intra = if cnt > 0 { per_sample_intra / cnt as f32 } else { 1.0 };

        repo::add_voice_profile_sample(
            pool,
            &NewVoiceProfileSample {
                profile_id,
                embedding: &emb_bytes,
                duration_ms: s.duration_ms as i64,
                snr_db: s.snr_db,
                intra_similarity: intra,
                meeting_id: None,
                source: "enrollment",
            },
        )?;
    }

    info!(
        profile_id,
        name = %display_name,
        first_name = %identity.first_name,
        samples = selected_indices.len(),
        reliability = reliability_score,
        "Voice profile enrolled"
    );

    Ok(EnrollmentResult {
        profile_id,
        name: display_name,
        samples_accepted: selected_indices.len(),
        samples_rejected: samples.len() - selected_indices.len(),
        reliability_score,
        centroid,
    })
}

/// Match nowego embeddingu do wszystkich enrolled profiles w DB.
/// Zwraca najlepszy match lub None jesli zaden nie przekracza progu.
///
/// Strategia: kombinacja centroid similarity + top-5 mean similarity ze wszystkimi
/// samples profilu. Top-K jest odporne na outliery lepiej niz max.
pub fn match_to_profiles(
    pool: &DbPool,
    embedding: &[f32],
) -> Result<Option<MatchResult>> {
    let profiles = repo::list_voice_profiles(pool)?;
    if profiles.is_empty() {
        return Ok(None);
    }

    let mut best: Option<MatchResult> = None;

    for profile in &profiles {
        let samples = repo::list_voice_profile_samples(pool, profile.id)?;
        if samples.is_empty() {
            continue;
        }

        // Porownanie z centroidem
        let centroid = bytes_to_embedding(&profile.centroid);
        let centroid_sim = cosine_similarity(embedding, &centroid);

        // Porownanie z wszystkimi samples
        let mut sims: Vec<f32> = samples
            .iter()
            .map(|s| {
                let emb = bytes_to_embedding(&s.embedding);
                cosine_similarity(embedding, &emb)
            })
            .collect();
        sims.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

        let max_sim = sims[0];
        let k = 5.min(sims.len());
        let topk_mean: f32 = sims.iter().take(k).sum::<f32>() / k as f32;

        // Combined score: 60% centroid + 40% topK mean
        //   - centroid jest stabilny (gladka reprezentacja)
        //   - topK mean wzmacnia gdy nowy embedding pasuje do kilku historycznych
        let score = 0.6 * centroid_sim + 0.4 * topk_mean;

        let this_result = MatchResult {
            profile_id: profile.id,
            profile_name: profile.name.clone(),
            score,
            confidence: MatchConfidence::from_score(score),
            centroid_similarity: centroid_sim,
            topk_mean,
            max_similarity: max_sim,
        };

        debug!(
            profile = %profile.name,
            score,
            centroid_sim,
            topk_mean,
            max_sim,
            "Profile match candidate"
        );

        if best.as_ref().map(|b| this_result.score > b.score).unwrap_or(true) {
            best = Some(this_result);
        }
    }

    // Zwracamy tylko gdy jest to faktyczny match (nie NoMatch)
    if let Some(ref b) = best {
        if b.confidence == MatchConfidence::NoMatch {
            return Ok(None);
        }
    }

    Ok(best)
}

/// Dodaje nowy sample do istniejacego profilu (incremental learning).
/// Przelicza centroid i reliability score.
pub fn add_sample_to_profile(
    pool: &DbPool,
    profile_id: i64,
    new_embedding: &[f32],
    duration_ms: u64,
    snr_db: f32,
    meeting_id: Option<&str>,
    source: &str,
) -> Result<()> {
    // Sprawdz ile samples juz jest
    let existing = repo::list_voice_profile_samples(pool, profile_id)?;
    if existing.len() as i64 >= MAX_SAMPLES_PER_PROFILE {
        debug!(
            profile_id,
            count = existing.len(),
            "Max samples reached, skipping incremental learn"
        );
        return Ok(());
    }

    // Policz intra_similarity nowego sample vs istniejace
    let mut sum = 0.0_f32;
    let mut cnt = 0;
    for s in &existing {
        let emb = bytes_to_embedding(&s.embedding);
        sum += cosine_similarity(new_embedding, &emb);
        cnt += 1;
    }
    let intra = if cnt > 0 { sum / cnt as f32 } else { 1.0 };

    // Odrzuc jesli nowy sample ma niska spojnosc z istniejacymi (ochrona przed
    // dryftem profilu przy false positive match)
    if intra < MIN_INTRA_SIMILARITY {
        warn!(
            profile_id,
            intra,
            "Incremental sample rejected — low coherence with existing"
        );
        return Ok(());
    }

    let emb_bytes = embedding_to_bytes(new_embedding);
    repo::add_voice_profile_sample(
        pool,
        &NewVoiceProfileSample {
            profile_id,
            embedding: &emb_bytes,
            duration_ms: duration_ms as i64,
            snr_db,
            intra_similarity: intra,
            meeting_id,
            source,
        },
    )?;

    // Przelicz centroid + reliability (load all samples po dodaniu)
    let updated = repo::list_voice_profile_samples(pool, profile_id)?;
    let embeddings: Vec<Vec<f32>> = updated
        .iter()
        .map(|s| bytes_to_embedding(&s.embedding))
        .collect();
    let new_centroid = compute_centroid(&embeddings);
    let new_reliability = intra_similarity(&embeddings);
    let new_count = updated.len() as i64;

    let centroid_bytes = embedding_to_bytes(&new_centroid);
    repo::update_voice_profile_stats(pool, profile_id, &centroid_bytes, new_count, new_reliability)?;

    debug!(
        profile_id,
        new_count,
        new_reliability,
        "Profile updated via incremental learning"
    );

    Ok(())
}

/// Po detekcji matchu oznacza profil jako aktywny i opcjonalnie robi incremental learn.
pub fn on_confident_match(
    pool: &DbPool,
    match_result: &MatchResult,
    embedding: &[f32],
    duration_ms: u64,
    snr_db: f32,
    meeting_id: Option<&str>,
) -> Result<()> {
    // Update last_seen + total_utterances
    repo::touch_voice_profile(pool, match_result.profile_id)
        .context("touch_voice_profile failed")?;

    // Incremental learning — tylko gdy very confident match i dobry SNR
    if match_result.score >= INCREMENTAL_LEARN_THRESHOLD && snr_db >= INCREMENTAL_MIN_SNR {
        add_sample_to_profile(
            pool,
            match_result.profile_id,
            embedding,
            duration_ms,
            snr_db,
            meeting_id,
            "incremental",
        )?;
    }

    Ok(())
}

/// Lista wszystkich profili w formacie API-friendly
pub fn list_profiles(pool: &DbPool) -> Result<Vec<ProfileInfo>> {
    let profiles = repo::list_voice_profiles(pool)?;
    Ok(profiles.into_iter().map(profile_to_info).collect())
}

/// Helper: konwersja DbVoiceProfile → ProfileInfo
pub fn profile_to_info(p: crate::db::models::DbVoiceProfile) -> ProfileInfo {
    ProfileInfo {
        id: p.id,
        name: p.name,
        first_name: p.first_name,
        last_name: p.last_name,
        nickname: p.nickname,
        sample_count: p.sample_count as usize,
        reliability_score: p.reliability_score,
        source: p.source,
        enrolled_at: p.enrolled_at,
        last_seen_at: p.last_seen_at,
        total_utterances: p.total_utterances as usize,
    }
}

/// Lekki DTO dla API / LLM
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProfileInfo {
    pub id: i64,
    /// Computed display name — "Jan Kowalski (janek)"
    pub name: String,
    pub first_name: String,
    pub last_name: Option<String>,
    pub nickname: Option<String>,
    pub sample_count: usize,
    pub reliability_score: f32,
    pub source: String,
    pub enrolled_at: String,
    pub last_seen_at: Option<String>,
    pub total_utterances: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy(dim: usize, seed: f32) -> Vec<f32> {
        (0..dim).map(|i| (i as f32 * 0.01 + seed).sin()).collect()
    }

    #[test]
    fn cosine_similarity_identical() {
        let a = dummy(192, 0.0);
        let sim = cosine_similarity(&a, &a);
        assert!((sim - 1.0).abs() < 1e-5);
    }

    #[test]
    fn cosine_similarity_different() {
        let a = dummy(192, 0.0);
        let b = dummy(192, 1.0);
        let sim = cosine_similarity(&a, &b);
        assert!(sim < 0.999);
    }

    #[test]
    fn centroid_average() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let centroid = compute_centroid(&[a, b]);
        // Centroid powinien byc [0.707, 0.707, 0.0] po L2 norm
        let expected = 1.0_f32 / 2.0_f32.sqrt();
        assert!((centroid[0] - expected).abs() < 1e-5);
        assert!((centroid[1] - expected).abs() < 1e-5);
    }

    #[test]
    fn intra_similarity_identical_samples() {
        let sample = dummy(192, 0.5);
        let samples = vec![sample.clone(), sample.clone(), sample];
        let intra = intra_similarity(&samples);
        assert!((intra - 1.0).abs() < 1e-5);
    }

    #[test]
    fn embedding_roundtrip() {
        let orig = dummy(192, 0.3);
        let bytes = embedding_to_bytes(&orig);
        let restored = bytes_to_embedding(&bytes);
        for i in 0..orig.len() {
            assert!((orig[i] - restored[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn build_profile_rejects_few_samples() {
        let samples = vec![
            EnrollmentSample {
                embedding: dummy(192, 0.0),
                duration_ms: 3000,
                snr_db: 20.0,
                rms: 0.1,
            },
            EnrollmentSample {
                embedding: dummy(192, 0.01),
                duration_ms: 3000,
                snr_db: 20.0,
                rms: 0.1,
            },
        ];
        let result = build_profile_from_samples(&samples);
        assert!(matches!(
            result,
            Err(EnrollmentError::TooFewSamples { .. })
        ));
    }

    #[test]
    fn build_profile_rejects_short_audio() {
        // window=100ms, hop=750ms → span dla 3 samples = 100 + 2*750 = 1600ms < 4000ms
        let samples: Vec<EnrollmentSample> = (0..3)
            .map(|i| EnrollmentSample {
                embedding: dummy(192, i as f32 * 0.01),
                duration_ms: 100, // bardzo krotkie okno
                snr_db: 20.0,
                rms: 0.1,
            })
            .collect();
        let result = build_profile_from_samples(&samples);
        assert!(
            matches!(result, Err(EnrollmentError::TooShortAudio { .. })),
            "should reject short-audio: {result:?}"
        );
    }

    #[test]
    fn build_profile_accepts_good_samples() {
        let samples: Vec<EnrollmentSample> = (0..5)
            .map(|i| EnrollmentSample {
                embedding: dummy(192, i as f32 * 0.005),
                duration_ms: 3000,
                snr_db: 25.0,
                rms: 0.1,
            })
            .collect();
        let result = build_profile_from_samples(&samples);
        assert!(result.is_ok(), "Should accept good samples: {:?}", result);
        let (centroid, reliability, indices) = result.unwrap();
        assert_eq!(centroid.len(), 192);
        assert!(reliability > 0.5);
        assert_eq!(indices.len(), 5);
    }

    #[test]
    fn person_identity_display_name_variants() {
        let full = PersonIdentity::new("Jan")
            .with_last_name("Kowalski")
            .with_nickname("janek");
        assert_eq!(full.display_name(), "Jan Kowalski (janek)");

        let last_only = PersonIdentity::new("Jan").with_last_name("Kowalski");
        assert_eq!(last_only.display_name(), "Jan Kowalski");

        let nick_only = PersonIdentity::new("Jan").with_nickname("janek");
        assert_eq!(nick_only.display_name(), "Jan (janek)");

        let first_only = PersonIdentity::new("Jan");
        assert_eq!(first_only.display_name(), "Jan");

        // Empty last/nick traktowane jako brak
        let empty_extras = PersonIdentity::new("Jan")
            .with_last_name("")
            .with_nickname("   ");
        assert_eq!(empty_extras.display_name(), "Jan");
    }

    #[test]
    fn person_identity_validation() {
        assert!(PersonIdentity::new("Jan").validate().is_ok());
        assert!(PersonIdentity::new("   ").validate().is_err());
        assert!(PersonIdentity::new("").validate().is_err());
    }

    #[test]
    fn match_confidence_thresholds() {
        assert_eq!(MatchConfidence::from_score(0.80), MatchConfidence::VeryConfident);
        assert_eq!(MatchConfidence::from_score(0.60), MatchConfidence::Confident);
        assert_eq!(MatchConfidence::from_score(0.50), MatchConfidence::Uncertain);
        assert_eq!(MatchConfidence::from_score(0.30), MatchConfidence::NoMatch);
    }

    #[test]
    fn snr_estimate_silence_is_very_low() {
        // Cisza daje signal=0, noise=floor (1e-6) → 20*log10(0) = -inf
        let silence = vec![0.0_f32; 16000];
        let snr = estimate_snr_db(&silence);
        // Dowolna wartosc bardzo niska lub -inf lub NaN jest OK
        assert!(snr.is_nan() || snr < -50.0, "silence SNR = {snr}");
    }

    #[test]
    fn snr_estimate_clean_signal_is_high() {
        // Czysty sinusoid ma wysoki SNR (signal >> background)
        let mut samples = vec![0.0_f32; 16000];
        for (i, s) in samples.iter_mut().enumerate() {
            *s = (i as f32 * 0.1).sin() * 0.5;
        }
        let snr = estimate_snr_db(&samples);
        assert!(snr > 0.0, "clean signal SNR should be positive: {snr}");
    }

    /// End-to-end test enrollment + match z rzeczywistego audio.
    /// Wymaga /tmp/sample_voices.wav z dwoma mowcami.
    ///
    /// Uruchom: DIARIZATION_MODEL_PATH=../models/diarization/embedding.onnx \
    ///   cargo test --lib --features inference-diarization voice_profile::tests::enrollment_flow \
    ///   -- --nocapture --ignored
    #[test]
    #[ignore]
    fn enrollment_flow_end_to_end() {
        use crate::db::{migrations, DbPool};
        use rusqlite::Connection;
        use std::sync::{Arc, Mutex};

        // 1. In-memory DB z migracjami
        let conn = Connection::open_in_memory().expect("open db");
        migrations::run(&conn).expect("run migrations");
        let pool: DbPool = Arc::new(Mutex::new(conn));

        // 2. Wczytaj audio — glos 1 (0-4.5s), glos 2 (5-end)
        let samples = read_wav_s16_mono_16k_priv("/tmp/sample_voices.wav").expect("wav");
        let glos1_i16 = &samples[0..16000 * 9 / 2];
        let glos2_i16 = &samples[5 * 16000..];

        let pcm1: Vec<u8> = glos1_i16.iter().flat_map(|&s| s.to_le_bytes()).collect();
        let pcm2: Vec<u8> = glos2_i16.iter().flat_map(|&s| s.to_le_bytes()).collect();

        // 3. Enrollment głosu 1 jako "Jan Kowalski (janek)"
        let identity1 = PersonIdentity::new("Jan")
            .with_last_name("Kowalski")
            .with_nickname("janek");
        let result = crate::diarization::service::enroll_profile_from_pcm(
            &pool, &identity1, &pcm1, "test",
        );
        println!("Enrollment result: {:?}", result);
        let enrollment = result.expect("enrollment should succeed");
        assert!(enrollment.samples_accepted >= 3);
        assert!(enrollment.reliability_score > 0.5);
        assert_eq!(enrollment.name, "Jan Kowalski (janek)");

        // 4. Identification głosu 1 → powinien match Jan Kowalski
        let samples_f32_1 = pcm_i16_le_to_f32(&pcm1);
        let ext_path = std::env::var("DIARIZATION_MODEL_PATH")
            .unwrap_or_else(|_| "../models/diarization/embedding.onnx".to_string());
        let ext = crate::diarization::embedding::EmbeddingExtractor::new(&ext_path)
            .expect("model load");
        let mid = samples_f32_1.len() / 2;
        let clip1 = &samples_f32_1[mid.saturating_sub(12000)..mid + 12000];
        let emb1 = ext.extract(clip1).expect("extract 1");
        let match1 = match_to_profiles(&pool, &emb1).expect("match");
        assert!(match1.is_some(), "glos 1 powinien sie dopasowac");
        let m1 = match1.unwrap();
        assert_eq!(m1.profile_name, "Jan Kowalski (janek)");
        assert!(m1.confidence.is_match());
        println!("Glos 1 → {} (score {:.3})", m1.profile_name, m1.score);

        // 5. Identification głosu 2 → NIE powinien match Jana (to inny speaker)
        let samples_f32_2 = pcm_i16_le_to_f32(&pcm2);
        let mid2 = samples_f32_2.len() / 2;
        let clip2 = &samples_f32_2[mid2.saturating_sub(12000)..mid2 + 12000];
        let emb2 = ext.extract(clip2).expect("extract 2");
        let match2 = match_to_profiles(&pool, &emb2).expect("match");
        if let Some(m2) = match2 {
            println!(
                "Glos 2 vs profile match: {} score={:.3} (should NOT be strong match)",
                m2.profile_name, m2.score
            );
            // Cross-speaker score powinien byc znacznie nizszy
            assert!(m2.score < m1.score, "glos 2 vs profil glos 1 powinien miec nizszy score");
            // Jesli to fałszywy match (score >= MATCH_CONFIDENT), test fail
            assert!(
                m2.score < MATCH_CONFIDENT || !m2.confidence.is_match(),
                "FALSE POSITIVE: glos 2 dopasowany do profilu glos 1 ze score {:.3}",
                m2.score
            );
        } else {
            println!("Glos 2 → brak match (correctly rejected)");
        }

        // 6. Enrollment głosu 2 jako "Anna Nowak" (bez nick)
        let identity2 = PersonIdentity::new("Anna").with_last_name("Nowak");
        let result2 = crate::diarization::service::enroll_profile_from_pcm(
            &pool, &identity2, &pcm2, "test",
        );
        println!("Enrollment result 2: {:?}", result2);
        let e2 = result2.expect("drugi enrollment powinien sie udac");
        assert_eq!(e2.name, "Anna Nowak");

        // 7. Teraz oba glosy powinny sie dopasowac do swoich profili
        let match1b = match_to_profiles(&pool, &emb1).expect("match").unwrap();
        assert_eq!(match1b.profile_name, "Jan Kowalski (janek)");
        assert!(match1b.confidence.is_match());

        let match2b = match_to_profiles(&pool, &emb2).expect("match").unwrap();
        assert_eq!(match2b.profile_name, "Anna Nowak");
        assert!(match2b.confidence.is_match());

        // 8. Sprawdz ze profile maja poprawnie rozbite pola first/last/nickname
        let profiles = list_profiles(&pool).expect("list");
        let jan = profiles.iter().find(|p| p.first_name == "Jan").expect("Jan");
        assert_eq!(jan.first_name, "Jan");
        assert_eq!(jan.last_name.as_deref(), Some("Kowalski"));
        assert_eq!(jan.nickname.as_deref(), Some("janek"));
        let anna = profiles.iter().find(|p| p.first_name == "Anna").expect("Anna");
        assert_eq!(anna.last_name.as_deref(), Some("Nowak"));
        assert_eq!(anna.nickname, None);

        println!("=== Enrollment flow OK ===");
        println!("  Jan Kowalski (janek) → score {:.3}", match1b.score);
        println!("  Anna Nowak           → score {:.3}", match2b.score);
    }

    fn read_wav_s16_mono_16k_priv(path: &str) -> Result<Vec<i16>, String> {
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
