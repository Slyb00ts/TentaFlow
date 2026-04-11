// =============================================================================
// Plik: api/dashboard/api_voice_profiles.rs
// Opis: HTTP API dla voice profiles (bulletproof speaker recognition).
//       Endpoints zaprojektowane do wolania przez LLM kiedy wykryje
//       introducje uzytkownika ("Czesc, tu Jan") albo przez wewnetrzne toole.
//
//       Routes:
//         GET    /api/voice-profiles              → lista profili
//         GET    /api/voice-profiles/:id          → szczegoly profilu
//         POST   /api/voice-profiles/enroll       → enrollment z PCM
//         POST   /api/voice-profiles/:id/append   → dodaj sample do istniejacego
//         POST   /api/voice-profiles/identify     → match PCM vs wszystkie profile
//         PATCH  /api/voice-profiles/:id          → rename
//         DELETE /api/voice-profiles/:id          → usun profil
//         GET    /api/voice-profiles/:id/samples  → lista samples dla debugu
//         POST   /api/voice-profiles/:id/assign-temp-speaker
//            — przypisz temp speakera z meetingu do profilu (LLM post-processing)
//         POST   /api/voice-profiles/cleanup-temp-speakers
//            — housekeeping
// =============================================================================

use crate::db::DbPool;
use hyper::Method;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};

fn json_error(msg: &str) -> String {
    serde_json::json!({ "error": msg }).to_string()
}

fn json_ok<T: Serialize>(value: T) -> String {
    serde_json::to_string(&value).unwrap_or_else(|e| json_error(&format!("serialize: {}", e)))
}

// ============================ Request DTOs ==================================

#[derive(Debug, Deserialize)]
struct EnrollRequest {
    /// Imie osoby (wymagane).
    first_name: String,
    /// Nazwisko (opcjonalne).
    #[serde(default)]
    last_name: Option<String>,
    /// Nick (opcjonalny).
    #[serde(default)]
    nickname: Option<String>,
    /// PCM 16-bit LE mono 16kHz, base64-encoded. Preferowane 15-60s czystej mowy.
    #[serde(rename = "audio_pcm_base64")]
    audio_pcm_base64: String,
    /// Zrodlo enrollment: "llm" | "manual" | "api" (default "api")
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AppendRequest {
    #[serde(rename = "audio_pcm_base64")]
    audio_pcm_base64: String,
    #[serde(default)]
    meeting_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IdentifyRequest {
    #[serde(rename = "audio_pcm_base64")]
    audio_pcm_base64: String,
    #[serde(default)]
    meeting_id: Option<String>,
}

/// PATCH body — pozwala zmienic czesci osobowe (first_name, last_name, nickname).
/// Wszystkie pola opcjonalne; display name (`name`) jest przeliczany.
#[derive(Debug, Deserialize)]
struct UpdateIdentityRequest {
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    last_name: Option<Option<String>>, // double-option: pominiete vs explicit null
    #[serde(default)]
    nickname: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
struct AssignTempSpeakerRequest {
    temp_speaker_id: i64,
}

// ============================ Response DTOs =================================

#[derive(Debug, Serialize)]
struct EnrollResponse {
    profile_id: i64,
    name: String,
    samples_accepted: usize,
    samples_rejected: usize,
    reliability_score: f32,
}

#[derive(Debug, Serialize)]
struct IdentifyResponse {
    matched: bool,
    profile_id: Option<i64>,
    profile_name: Option<String>,
    score: f32,
    confidence: String,
    centroid_similarity: f32,
    topk_mean: f32,
    max_similarity: f32,
}

#[derive(Debug, Serialize)]
struct AppendResponse {
    samples_added: usize,
}

// ============================ Router ========================================

pub fn route_voice_profiles_api(
    method: &Method,
    path: &str,
    query_string: &str,
    db: &DbPool,
    body: &[u8],
) -> (u16, String) {
    let _ = query_string;

    // GET /api/voice-profiles
    if path == "/api/voice-profiles" && *method == Method::GET {
        return handle_list(db);
    }

    // POST /api/voice-profiles/enroll
    if path == "/api/voice-profiles/enroll" && *method == Method::POST {
        return handle_enroll(db, body);
    }

    // POST /api/voice-profiles/identify
    if path == "/api/voice-profiles/identify" && *method == Method::POST {
        return handle_identify(db, body);
    }

    // POST /api/voice-profiles/cleanup-temp-speakers
    if path == "/api/voice-profiles/cleanup-temp-speakers" && *method == Method::POST {
        return handle_cleanup_temp(db);
    }

    // /api/voice-profiles/:id/*
    if let Some(rest) = path.strip_prefix("/api/voice-profiles/") {
        if let Some((id_str, action)) = rest.split_once('/') {
            let id = match id_str.parse::<i64>() {
                Ok(i) => i,
                Err(_) => return (400, json_error("invalid profile id")),
            };
            match (method.clone(), action) {
                (Method::POST, "append") => return handle_append(db, id, body),
                (Method::POST, "assign-temp-speaker") => {
                    return handle_assign_temp_speaker(db, id, body);
                }
                (Method::GET, "samples") => return handle_list_samples(db, id),
                _ => return (404, json_error("voice-profiles route not found")),
            }
        }
        // /api/voice-profiles/:id (no trailing path)
        let id = match rest.parse::<i64>() {
            Ok(i) => i,
            Err(_) => return (400, json_error("invalid profile id")),
        };
        match *method {
            Method::GET => return handle_get(db, id),
            Method::PATCH => return handle_rename(db, id, body),
            Method::DELETE => return handle_delete(db, id),
            _ => return (405, json_error("method not allowed")),
        }
    }

    (404, json_error("voice-profiles route not found"))
}

// ============================ Handlers ======================================

fn handle_list(db: &DbPool) -> (u16, String) {
    match crate::diarization::list_profiles(db) {
        Ok(profiles) => (200, json_ok(profiles)),
        Err(e) => (500, json_error(&format!("list failed: {}", e))),
    }
}

fn handle_get(db: &DbPool, id: i64) -> (u16, String) {
    match crate::db::repository::get_voice_profile(db, id) {
        Ok(Some(p)) => {
            let info = crate::diarization::voice_profile::profile_to_info(p);
            (200, json_ok(info))
        }
        Ok(None) => (404, json_error("profile not found")),
        Err(e) => (500, json_error(&format!("db error: {}", e))),
    }
}

fn handle_enroll(db: &DbPool, body: &[u8]) -> (u16, String) {
    let req: EnrollRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => return (400, json_error(&format!("invalid JSON: {}", e))),
    };

    if req.first_name.trim().is_empty() {
        return (400, json_error("first_name cannot be empty"));
    }

    let pcm_bytes = match BASE64.decode(req.audio_pcm_base64.as_bytes()) {
        Ok(b) => b,
        Err(e) => return (400, json_error(&format!("base64 decode: {}", e))),
    };

    let source = req.source.as_deref().unwrap_or("api");

    let mut identity = crate::diarization::voice_profile::PersonIdentity::new(&req.first_name);
    if let Some(ref last) = req.last_name {
        identity = identity.with_last_name(last);
    }
    if let Some(ref nick) = req.nickname {
        identity = identity.with_nickname(nick);
    }

    match crate::diarization::service::enroll_profile_from_pcm(db, &identity, &pcm_bytes, source) {
        Ok(result) => {
            let resp = EnrollResponse {
                profile_id: result.profile_id,
                name: result.name,
                samples_accepted: result.samples_accepted,
                samples_rejected: result.samples_rejected,
                reliability_score: result.reliability_score,
            };
            (200, json_ok(resp))
        }
        Err(e) => (400, json_error(&e)),
    }
}

fn handle_append(db: &DbPool, profile_id: i64, body: &[u8]) -> (u16, String) {
    let req: AppendRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => return (400, json_error(&format!("invalid JSON: {}", e))),
    };

    // Sprawdz ze profil istnieje
    match crate::db::repository::get_voice_profile(db, profile_id) {
        Ok(Some(_)) => {}
        Ok(None) => return (404, json_error("profile not found")),
        Err(e) => return (500, json_error(&format!("db error: {}", e))),
    }

    let pcm_bytes = match BASE64.decode(req.audio_pcm_base64.as_bytes()) {
        Ok(b) => b,
        Err(e) => return (400, json_error(&format!("base64 decode: {}", e))),
    };

    match crate::diarization::service::append_to_profile_from_pcm(
        db,
        profile_id,
        &pcm_bytes,
        req.meeting_id.as_deref(),
    ) {
        Ok(added) => (200, json_ok(AppendResponse { samples_added: added })),
        Err(e) => (400, json_error(&e)),
    }
}

fn handle_identify(db: &DbPool, body: &[u8]) -> (u16, String) {
    let req: IdentifyRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => return (400, json_error(&format!("invalid JSON: {}", e))),
    };

    let pcm_bytes = match BASE64.decode(req.audio_pcm_base64.as_bytes()) {
        Ok(b) => b,
        Err(e) => return (400, json_error(&format!("base64 decode: {}", e))),
    };

    // Wyciagamy embedding + robimy full match z profile DB
    let samples_f32 = crate::diarization::pcm_i16_le_to_f32(&pcm_bytes);
    if samples_f32.is_empty() {
        return (400, json_error("empty audio"));
    }

    // Wymagany model
    let ext = match crate::diarization::embedding::EmbeddingExtractor::new(
        &std::env::var("DIARIZATION_MODEL_PATH")
            .unwrap_or_else(|_| "models/diarization/embedding.onnx".to_string()),
    ) {
        Ok(e) => e,
        Err(e) => return (500, json_error(&format!("model load: {}", e))),
    };

    // Clip to middle 1.5s for stable + fast
    let max_samples = 24000;
    let clipped: &[f32] = if samples_f32.len() > max_samples {
        let start = (samples_f32.len() - max_samples) / 2;
        &samples_f32[start..start + max_samples]
    } else {
        &samples_f32[..]
    };

    let embedding = match ext.extract(clipped) {
        Ok(e) => e,
        Err(e) => return (500, json_error(&format!("extract: {}", e))),
    };

    match crate::diarization::match_to_profiles(db, &embedding) {
        Ok(Some(result)) => {
            let confidence_str = format!("{:?}", result.confidence).to_lowercase();
            let resp = IdentifyResponse {
                matched: result.confidence.is_match(),
                profile_id: Some(result.profile_id),
                profile_name: Some(result.profile_name),
                score: result.score,
                confidence: confidence_str,
                centroid_similarity: result.centroid_similarity,
                topk_mean: result.topk_mean,
                max_similarity: result.max_similarity,
            };
            (200, json_ok(resp))
        }
        Ok(None) => {
            let resp = IdentifyResponse {
                matched: false,
                profile_id: None,
                profile_name: None,
                score: 0.0,
                confidence: "nomatch".to_string(),
                centroid_similarity: 0.0,
                topk_mean: 0.0,
                max_similarity: 0.0,
            };
            (200, json_ok(resp))
        }
        Err(e) => (500, json_error(&format!("match failed: {}", e))),
    }
}

fn handle_rename(db: &DbPool, id: i64, body: &[u8]) -> (u16, String) {
    let req: UpdateIdentityRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => return (400, json_error(&format!("invalid JSON: {}", e))),
    };

    let existing = match crate::db::repository::get_voice_profile(db, id) {
        Ok(Some(p)) => p,
        Ok(None) => return (404, json_error("profile not found")),
        Err(e) => return (500, json_error(&format!("db error: {}", e))),
    };

    // Merge incoming changes nad istniejacym profilem
    let new_first = req.first_name.as_deref().map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing.first_name)
        .to_string();

    let new_last: Option<String> = match req.last_name {
        Some(Some(v)) => {
            let trimmed = v.trim().to_string();
            if trimmed.is_empty() { None } else { Some(trimmed) }
        }
        Some(None) => None, // explicit null → clear
        None => existing.last_name.clone(),
    };

    let new_nick: Option<String> = match req.nickname {
        Some(Some(v)) => {
            let trimmed = v.trim().to_string();
            if trimmed.is_empty() { None } else { Some(trimmed) }
        }
        Some(None) => None,
        None => existing.nickname.clone(),
    };

    if new_first.is_empty() {
        return (400, json_error("first_name cannot be empty"));
    }

    // Recompute display name
    let identity = {
        let mut i = crate::diarization::voice_profile::PersonIdentity::new(&new_first);
        if let Some(ref l) = new_last {
            i = i.with_last_name(l);
        }
        if let Some(ref n) = new_nick {
            i = i.with_nickname(n);
        }
        i
    };
    let new_display_name = identity.display_name();

    match crate::db::repository::update_voice_profile_identity(
        db,
        id,
        &new_display_name,
        &new_first,
        new_last.as_deref(),
        new_nick.as_deref(),
    ) {
        Ok(()) => (
            200,
            json_ok(serde_json::json!({
                "ok": true,
                "name": new_display_name,
                "first_name": new_first,
                "last_name": new_last,
                "nickname": new_nick,
            })),
        ),
        Err(e) => (500, json_error(&format!("update failed: {}", e))),
    }
}

fn handle_delete(db: &DbPool, id: i64) -> (u16, String) {
    match crate::db::repository::delete_voice_profile(db, id) {
        Ok(()) => (200, json_ok(serde_json::json!({"ok": true}))),
        Err(e) => (500, json_error(&format!("delete failed: {}", e))),
    }
}

fn handle_list_samples(db: &DbPool, profile_id: i64) -> (u16, String) {
    match crate::db::repository::list_voice_profile_samples(db, profile_id) {
        Ok(samples) => {
            // Nie wysylamy raw embeddingow (duzo danych, nieczytelne),
            // tylko metadata do debugowania
            let summary: Vec<_> = samples
                .into_iter()
                .map(|s| {
                    serde_json::json!({
                        "id": s.id,
                        "duration_ms": s.duration_ms,
                        "snr_db": s.snr_db,
                        "intra_similarity": s.intra_similarity,
                        "source": s.source,
                        "meeting_id": s.meeting_id,
                        "created_at": s.created_at,
                    })
                })
                .collect();
            (200, json_ok(summary))
        }
        Err(e) => (500, json_error(&format!("list samples failed: {}", e))),
    }
}

fn handle_assign_temp_speaker(db: &DbPool, profile_id: i64, body: &[u8]) -> (u16, String) {
    let req: AssignTempSpeakerRequest = match serde_json::from_slice(body) {
        Ok(r) => r,
        Err(e) => return (400, json_error(&format!("invalid JSON: {}", e))),
    };

    // Sprawdz ze profil istnieje
    if crate::db::repository::get_voice_profile(db, profile_id)
        .ok()
        .flatten()
        .is_none()
    {
        return (404, json_error("profile not found"));
    }

    match crate::db::repository::assign_temp_speaker_to_profile(db, req.temp_speaker_id, profile_id) {
        Ok(()) => (200, json_ok(serde_json::json!({"ok": true}))),
        Err(e) => (500, json_error(&format!("assign failed: {}", e))),
    }
}

fn handle_cleanup_temp(db: &DbPool) -> (u16, String) {
    // Usun temp speakers starsze niz 7 dni
    match crate::db::repository::cleanup_old_voice_temp_speakers(db, 7) {
        Ok(n) => (200, json_ok(serde_json::json!({"deleted": n}))),
        Err(e) => (500, json_error(&format!("cleanup failed: {}", e))),
    }
}
