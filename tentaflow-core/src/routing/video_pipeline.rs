// =============================================================================
// Plik: routing/video_pipeline.rs
// Opis: Pipeline rozpoznawania emocji + wieku + płci z klatki wideo uczestnika
//       meetingu. Wywoływany przez handler `MeetingEventPayload::VideoFrame`
//       w `reverse_request.rs`. Inferencja idzie przez `vision::registry`
//       (SCRFD → HSEmotion → MiVOLO), wynik leci broadcastem jako
//       `MeetingEventPayload::ParticipantAttributes`.
// =============================================================================

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

use parking_lot::Mutex;
use tracing::{debug, warn};

use tentaflow_protocol::{MeetingEventPayload, MeetingLiveEvent};

use crate::db::DbPool;
use crate::meeting::manager::{
    DEFAULT_VISION_AGE_ALIAS, DEFAULT_VISION_EMOTION_ALIAS, DEFAULT_VISION_FACE_ALIAS,
};
use crate::vision::hsemotion::EMOTION_LABELS;

/// Throttle: tylko jedna inferencja na 2 sekundy per uczestnik. Pipeline jest
/// CPU-bound (tract-onnx), więc bez throttla 1 fps bot zarzucałby host
/// trzema modelami × N uczestników. 2 s daje ~30 inferences/min/usera, a
/// EWMA i tak wygładza wynik między klatkami.
const THROTTLE_MS: u128 = 2_000;

/// EWMA współczynnik dla świeżego prawdopodobieństwa. Stała 0.6 tłumi szum
/// pojedynczej klatki bez znacznego latency — przy 1 inference/2s nowa
/// emocja konwerguje do prawidłowej etykiety w ~6 s (3 klatkach).
const EMOTION_EWMA_NEW: f32 = 0.6;

/// Padding dookoła bbox face crop'a. SCRFD zwraca ciasny bbox samej twarzy;
/// HSEmotion i MiVOLO trenowane były na większych wycinkach (więcej
/// kontekstu), więc dodajemy 20 % marginesu z każdej strony.
const FACE_CROP_PADDING: f32 = 0.20;

/// Stan throttle + smoothing per uczestnik. Klucz: `participant_id` z eventu
/// (DOM `data-tid` Teams). Wpisy żyją do końca procesu — meeting bot
/// startuje świeżą sesję z czystym stanem, bo po stop+start router idzie
/// dalej, ale uczestnicy meetingu nie pojawią się w nowej z tym samym id.
struct ParticipantState {
    last_inference_at: Instant,
    /// Wygładzony wektor prawdopodobieństw po EWMA — kolejność musi się
    /// pokrywać z `EMOTION_LABELS`. None gdy jeszcze nie było udanej
    /// klasyfikacji emocji dla tego uczestnika.
    emotion_smoothed: Option<[f32; 8]>,
}

fn states() -> &'static Mutex<HashMap<String, ParticipantState>> {
    static STATES: OnceLock<Mutex<HashMap<String, ParticipantState>>> = OnceLock::new();
    STATES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cache "alias jest pusty" — żeby nie spamować logiem przy każdym frame
/// gdy admin nigdy nie zdeployował modelu vision. Klucz: nazwa aliasu,
/// wartość: ostatni Instant kiedy zwróciliśmy debug log. 60 s rate-limit.
fn alias_warn_cache() -> &'static Mutex<HashMap<&'static str, Instant>> {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, Instant>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Zwraca true gdy upłynęło < 60 s od ostatniego logu — caller w tej sytuacji
/// pomija log. Bez tego konsola tonęłaby w wpisach przy włączonym bocie bez
/// zdeployowanego silnika vision (1 fps × N uczestników).
fn alias_warn_throttled(alias: &'static str) -> bool {
    let mut guard = alias_warn_cache().lock();
    let now = Instant::now();
    match guard.get(&alias) {
        Some(prev) if now.duration_since(*prev).as_secs() < 60 => true,
        _ => {
            guard.insert(alias, now);
            false
        }
    }
}

/// EWMA na wektorze prawdopodobieństw. Argmax na wygładzonym vectorze
/// zapobiega migotaniu ikonki przy granicy klas (np. neutral/sad sąsiadują
/// na ~0.4/0.4 — surowy argmax skacze, EWMA stabilizuje).
fn smooth_emotion(prev: Option<[f32; 8]>, fresh: [f32; 8]) -> [f32; 8] {
    match prev {
        None => fresh,
        Some(p) => {
            let mut out = [0f32; 8];
            for i in 0..8 {
                out[i] = EMOTION_EWMA_NEW * fresh[i] + (1.0 - EMOTION_EWMA_NEW) * p[i];
            }
            out
        }
    }
}

/// Główne wejście. Wołane synchronicznie przez handler MeetingEvent zaraz po
/// publikacji `VideoFrame` na broadcast bus. Spawnuje task tokio który robi
/// CPU-bound inference w `spawn_blocking` i publikuje wynik jako
/// `ParticipantAttributes` na ten sam bus.
///
/// Zwraca natychmiast — caller nie czeka na wynik inferencji (1 fps × N
/// uczestników nie może blokować reverse stream'u na ~50–200 ms inferencji).
pub fn maybe_spawn_inference(
    pool: DbPool,
    meeting_key: String,
    timestamp_ms: i64,
    participant_id: String,
    name: Option<String>,
    ts_ms: u64,
    jpeg: Vec<u8>,
) {
    // Throttle check pod mutexem — krótkim, bo nie blokujemy nic długiego.
    {
        let mut guard = states().lock();
        let entry = guard
            .entry(participant_id.clone())
            .or_insert(ParticipantState {
                // `Instant::now() - 1h` jako sentinel "dawno temu" — pierwsze
                // wywołanie zawsze przechodzi throttle bez special-case.
                last_inference_at: Instant::now() - std::time::Duration::from_secs(3600),
                emotion_smoothed: None,
            });
        let elapsed = entry.last_inference_at.elapsed().as_millis();
        if elapsed < THROTTLE_MS {
            return;
        }
        entry.last_inference_at = Instant::now();
    }

    tokio::spawn(async move {
        let result = run_inference(&pool, &participant_id, &jpeg).await;
        match result {
            Ok(Some(attrs)) => {
                tracing::debug!(
                    "ParticipantAttributes emit: participant_id={} name={:?} emotion={:?} conf={:?} age={:?} gender_male={:?}",
                    participant_id,
                    name,
                    attrs.emotion,
                    attrs.emotion_confidence,
                    attrs.age,
                    attrs.gender_male_prob,
                );
                let live_event = MeetingLiveEvent {
                    meeting_key,
                    timestamp_ms,
                    payload: MeetingEventPayload::ParticipantAttributes {
                        participant_id,
                        name,
                        ts_ms,
                        emotion: attrs.emotion,
                        emotion_confidence: attrs.emotion_confidence,
                        age: attrs.age,
                        gender_male_prob: attrs.gender_male_prob,
                    },
                };
                crate::dispatch::meeting_live_broadcast::publish(live_event);
            }
            Ok(None) => {
                // Pipeline świadomie pominięty (alias pusty / brak silnika).
            }
            Err(e) => {
                warn!(
                    "video_pipeline: inference dla '{}' nie powiodła się: {}",
                    participant_id, e
                );
            }
        }
    });
}

/// Wynik z inferencji w formacie 1:1 do wariantu `ParticipantAttributes`.
struct InferAttrs {
    emotion: Option<String>,
    emotion_confidence: Option<f32>,
    age: Option<f32>,
    gender_male_prob: Option<f32>,
}

/// Rozwiązuje alias do nazwy serwisu (czyli klucza w `vision::registry`).
/// Zwraca `Ok(None)` gdy alias pusty/brak — caller traktuje to jako "skip".
fn resolve_vision_alias(pool: &DbPool, alias: &'static str) -> anyhow::Result<Option<String>> {
    match crate::db::repository::resolve_model_alias(pool, alias)? {
        Some(a) if !a.target_model.trim().is_empty() => Ok(Some(a.target_model)),
        _ => {
            if !alias_warn_throttled(alias) {
                debug!(
                    "video_pipeline: alias '{}' pusty — pomijam vision pipeline",
                    alias
                );
            }
            Ok(None)
        }
    }
}

/// Pełen cykl: dekoduj JPEG → SCRFD → crop → HSEmotion + MiVOLO.
/// Brak twarzy w klatce → wciąż emitujemy `Some` z polami `None`, żeby GUI
/// mogło wyczyścić stare badge'a (uczestnik odwrócił głowę).
/// Zwraca `Ok(None)` tylko gdy face alias pusty (cały pipeline skip).
async fn run_inference(
    pool: &DbPool,
    participant_id: &str,
    jpeg: &[u8],
) -> anyhow::Result<Option<InferAttrs>> {
    let face_service = match resolve_vision_alias(pool, DEFAULT_VISION_FACE_ALIAS)? {
        Some(s) => s,
        None => return Ok(None),
    };
    let emotion_service = resolve_vision_alias(pool, DEFAULT_VISION_EMOTION_ALIAS)?;
    let age_service = resolve_vision_alias(pool, DEFAULT_VISION_AGE_ALIAS)?;

    // Cały blok inferencji (decode JPEG + 3 modele tract) jest CPU-bound.
    // Bez `spawn_blocking` zablokowałby tokio worker thread przez ~50–200 ms,
    // psując latency innych async tasks (chat streaming, mesh heartbeat).
    let jpeg_owned = jpeg.to_vec();
    let participant_id_owned = participant_id.to_string();
    let result = tokio::task::spawn_blocking(move || {
        run_inference_blocking(
            &participant_id_owned,
            &jpeg_owned,
            &face_service,
            emotion_service.as_deref(),
            age_service.as_deref(),
        )
    })
    .await
    .map_err(|e| anyhow::anyhow!("video_pipeline: spawn_blocking join: {}", e))??;

    Ok(Some(result))
}

/// Synchroniczny rdzeń inferencji — tract pracuje synchronicznie, więc
/// cała ścieżka: image::load_from_memory → SCRFD → crop → HSEmotion +
/// MiVOLO musi siedzieć w spawn_blocking po stronie wywołującego.
fn run_inference_blocking(
    participant_id: &str,
    jpeg: &[u8],
    face_service: &str,
    emotion_service: Option<&str>,
    age_service: Option<&str>,
) -> anyhow::Result<InferAttrs> {
    use image::ImageReader;
    use std::io::Cursor;

    // Dekoduj JPEG → RGB8. ImageReader z `with_guessed_format` rozpoznaje też
    // PNG/WebP gdyby bot kiedyś zmienił format; brak heurystyki na zawartość.
    let img = ImageReader::new(Cursor::new(jpeg))
        .with_guessed_format()?
        .decode()
        .map_err(|e| anyhow::anyhow!("video_pipeline: decode JPEG: {}", e))?
        .to_rgb8();
    let (w, h) = (img.width(), img.height());

    // Face detection.
    let detector = crate::vision::get_face_detector(face_service)
        .ok_or_else(|| anyhow::anyhow!(
            "video_pipeline: silnik face '{}' nie zarejestrowany — alias wskazuje na nieistniejący serwis",
            face_service
        ))?;
    let faces = detector.detect(img.as_raw(), w, h)?;

    // Wybierz twarz z najwyższym score — typowo jedna twarz per kafelek
    // Teams (jeden uczestnik), ale przy wąskim kadrze może wpaść druga.
    let best_face = faces.iter().max_by(|a, b| {
        a.score
            .partial_cmp(&b.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let Some(face) = best_face else {
        // Brak twarzy — wciąż chcemy wyemitować event z `None`, żeby
        // GUI wyczyściło stare badge'a (uczestnik odwrócił głowę / wyszedł
        // z kadru / zasłonił obiektyw).
        return Ok(InferAttrs {
            emotion: None,
            emotion_confidence: None,
            age: None,
            gender_male_prob: None,
        });
    };

    // Crop z paddingiem. Klampujemy do granic obrazka żeby nie wyjść poza
    // bufor (SCRFD czasami zwraca bbox lekko poza obrazem przy twarzy
    // przy krawędzi kafelka).
    let (x1, y1, x2, y2) = face.bbox;
    let bw = (x2 - x1).max(1.0);
    let bh = (y2 - y1).max(1.0);
    let pad_x = bw * FACE_CROP_PADDING;
    let pad_y = bh * FACE_CROP_PADDING;
    let cx1 = (x1 - pad_x).max(0.0) as u32;
    let cy1 = (y1 - pad_y).max(0.0) as u32;
    let cx2 = (x2 + pad_x).min(w as f32) as u32;
    let cy2 = (y2 + pad_y).min(h as f32) as u32;
    if cx2 <= cx1 || cy2 <= cy1 {
        return Ok(InferAttrs {
            emotion: None,
            emotion_confidence: None,
            age: None,
            gender_male_prob: None,
        });
    }
    let crop_w = cx2 - cx1;
    let crop_h = cy2 - cy1;
    // Sub-image bezkopiowo: image::SubImage pożycza piksele, ale do tract
    // potrzebujemy ciągłego bufora row-major. Przepakowujemy do Vec<u8>.
    let mut crop = Vec::with_capacity((crop_w * crop_h * 3) as usize);
    for y in cy1..cy2 {
        let row_start = ((y * w) + cx1) * 3;
        let row_end = ((y * w) + cx2) * 3;
        crop.extend_from_slice(&img.as_raw()[row_start as usize..row_end as usize]);
    }

    // Emotion: HSEmotion zwraca probabilities w kolejności EMOTION_LABELS.
    let (emotion_label, emotion_conf) = match emotion_service {
        Some(svc) => match crate::vision::get_emotion(svc) {
            Some(engine) => match engine.classify(&crop, crop_w, crop_h) {
                Ok(res) => {
                    let fresh = probs_to_array(&res.probabilities);
                    let mut guard = states().lock();
                    let st = guard
                        .entry(participant_id.to_string())
                        .or_insert(ParticipantState {
                            last_inference_at: Instant::now(),
                            emotion_smoothed: None,
                        });
                    let smoothed = smooth_emotion(st.emotion_smoothed, fresh);
                    st.emotion_smoothed = Some(smoothed);
                    let (idx, conf) = argmax(&smoothed);
                    (Some(EMOTION_LABELS[idx].to_string()), Some(conf))
                }
                Err(e) => {
                    warn!("video_pipeline: HSEmotion classify: {}", e);
                    (None, None)
                }
            },
            None => (None, None),
        },
        None => (None, None),
    };

    // Age + gender: MiVOLO zwraca oba.
    let (age, gender_male_prob) = match age_service {
        Some(svc) => match crate::vision::get_age_gender(svc) {
            Some(engine) => match engine.predict(&crop, crop_w, crop_h) {
                Ok(ag) => (Some(ag.age_years), Some(ag.gender_male_prob)),
                Err(e) => {
                    warn!("video_pipeline: MiVOLO predict: {}", e);
                    (None, None)
                }
            },
            None => (None, None),
        },
        None => (None, None),
    };

    Ok(InferAttrs {
        emotion: emotion_label,
        emotion_confidence: emotion_conf,
        age,
        gender_male_prob,
    })
}

/// Mapuje (label, prob) z HSEmotion na pozycyjny array zgodny z
/// `EMOTION_LABELS`. Jeśli HSEmotion zwróciło inny zestaw etykiet (np.
/// w przyszłości zmieni się model), brakujące pozycje dostaną 0.0.
fn probs_to_array(probs: &[(String, f32)]) -> [f32; 8] {
    let mut arr = [0f32; 8];
    for (label, prob) in probs {
        if let Some(idx) = EMOTION_LABELS.iter().position(|l| *l == label.as_str()) {
            arr[idx] = *prob;
        }
    }
    arr
}

fn argmax(arr: &[f32; 8]) -> (usize, f32) {
    let mut best_idx = 0;
    let mut best_val = arr[0];
    for (i, &v) in arr.iter().enumerate().skip(1) {
        if v > best_val {
            best_val = v;
            best_idx = i;
        }
    }
    (best_idx, best_val)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pierwszy fresh wektor → zwraca go bez modyfikacji (brak prev state).
    #[test]
    fn smooth_emotion_no_prev_returns_fresh() {
        let fresh = [0.1, 0.0, 0.0, 0.0, 0.7, 0.2, 0.0, 0.0];
        let out = smooth_emotion(None, fresh);
        assert_eq!(out, fresh);
    }

    /// EWMA: nowy = 0.6*fresh + 0.4*prev. Sprawdzamy konkretną liczbę żeby
    /// regression w stałej EMOTION_EWMA_NEW złapać.
    #[test]
    fn smooth_emotion_ewma_blends() {
        let prev = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let fresh = [0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0];
        let out = smooth_emotion(Some(prev), fresh);
        assert!((out[0] - 0.4).abs() < 1e-6, "out[0]={}", out[0]);
        assert!((out[4] - 0.6).abs() < 1e-6, "out[4]={}", out[4]);
    }

    /// Argmax wybiera najwyższe.
    #[test]
    fn argmax_picks_highest() {
        let arr = [0.1, 0.0, 0.05, 0.0, 0.4, 0.2, 0.25, 0.0];
        let (idx, val) = argmax(&arr);
        assert_eq!(idx, 4);
        assert!((val - 0.4).abs() < 1e-6);
    }

    /// probs_to_array przemapowuje po nazwach. Kolejność EMOTION_LABELS
    /// hardcoded: ["Anger", "Contempt", "Disgust", "Fear", "Happiness",
    /// "Neutral", "Sadness", "Surprise"].
    #[test]
    fn probs_to_array_maps_by_label() {
        let probs = vec![
            ("Happiness".to_string(), 0.8),
            ("Sadness".to_string(), 0.1),
            ("Neutral".to_string(), 0.1),
        ];
        let arr = probs_to_array(&probs);
        assert!((arr[4] - 0.8).abs() < 1e-6); // Happiness
        assert!((arr[5] - 0.1).abs() < 1e-6); // Neutral
        assert!((arr[6] - 0.1).abs() < 1e-6); // Sadness
        assert!((arr[0] - 0.0).abs() < 1e-6); // Anger nieobecne
    }
}
