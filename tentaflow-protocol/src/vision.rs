// =============================================================================
// Plik: vision.rs
// Opis: Typy protokolu dla inference vision (face detection / age+gender /
//       emotion). Spakowane jako jeden `VisionInferPayload` enum z 2 parami
//       request/response — zeby zaoszczedzic sloty w MessageBody (rkyv 0.8
//       ma twardy limit 256 wariantow w enumie). Patrn `ProfilingPayload`.
// =============================================================================

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// Format obrazka w wire — kodek decyduje klient. Server uzyje crate `image`
/// zeby zdekodowac do RgbImage. Surowy bufor RGB tez wspierany dla minimum
/// latency (po stronie meeting-bota juz mamy RGB w pamieci).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub enum VisionImageFormat {
    /// JPEG / PNG / WEBP — auto-detect po magic bytes.
    Encoded,
    /// Raw RGB row-major; wymaga `width` i `height` w request'ie.
    RawRgb { width: u32, height: u32 },
}

#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct VisionInferRequest {
    /// Nazwa zdeployowanego serwisu (klucz w `vision::registry`). Caller
    /// dostaje to z `service_name` zwroconej przez deploy handler.
    pub service_name: String,
    pub image: Vec<u8>,
    pub format: VisionImageFormat,
}

/// Bbox (x1, y1, x2, y2) w pikselach oryginalnego obrazka + score + opcjonalne
/// 5 keypointow (lewe oko, prawe oko, nos, lewy kacik ust, prawy kacik ust).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct VisionFaceDet {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub score: f32,
    /// `[(x, y); 5]` lub pusta lista gdy detector bez keypoints.
    pub keypoints: Vec<(f32, f32)>,
}

#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub enum VisionInferResult {
    Faces(Vec<VisionFaceDet>),
    AgeGender {
        age_years: f32,
        gender_male_prob: f32,
    },
    Emotion {
        label: String,
        probabilities: Vec<(String, f32)>,
        valence: Option<f32>,
        arousal: Option<f32>,
    },
}

#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct VisionInferResponse {
    pub service_name: String,
    pub result: VisionInferResult,
    pub latency_ms: u64,
}

/// Inner-enum pack — jeden slot w MessageBody. Patrz ProfilingPayload jako wzor.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub enum VisionInferPayload {
    InferRequest(VisionInferRequest),
    InferResponse(VisionInferResponse),
}
