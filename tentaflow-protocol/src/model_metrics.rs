// =============================================================================
// Plik: model_metrics.rs
// Opis: Binarny protokół CBOR dla odczytu metryk modeli (rollup histogramowy)
//       oraz cennika per-model. Agregacja mesh-wide liczona jest po stronie Core.
// Przykład: MessageBody::ModelMetricsBody(ModelMetricsPayload::SummaryRequest { .. })
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

/// Opcjonalny filtr wymiarów dla agregacji summary. Puste pole = brak
/// ograniczenia. `service` filtruje po stabilnym `service_key`.
#[derive(Debug, Clone, PartialEq, Default, SerdeSerialize, SerdeDeserialize)]
pub struct ModelMetricsFilterWire {
    pub model: Option<String>,
    pub node: Option<String>,
    pub service: Option<String>,
    pub backend: Option<String>,
    pub modality: Option<String>,
}

/// Jeden zagregowany wiersz summary. `key` zależy od `group_by` (user/group/
/// model/node/service/day). Percentyle wyliczane są z ZSUMOWANYCH histogramów
/// wszystkich wierszy w grupie; `None` = brak próbek w danym histogramie.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelMetricsRowWire {
    pub key: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub embedding_tokens: i64,
    pub audio_ms: i64,
    pub images: i64,
    pub request_count: i64,
    pub success_count: i64,
    pub error_count: i64,
    pub cost: f64,
    /// `true` gdy w grupie sa tokeny modeli BEZ wpisu w cenniku — pozwala UI
    /// odroznic "0 zl bo darmowy" od "brak cennika".
    pub missing_pricing: bool,
    pub error_rate: f64,
    pub ttft_p50: Option<f64>,
    pub ttft_p90: Option<f64>,
    pub ttft_p99: Option<f64>,
    pub decode_p50: Option<f64>,
    pub decode_p90: Option<f64>,
    pub decode_p99: Option<f64>,
    pub e2e_p50: Option<f64>,
    pub e2e_p90: Option<f64>,
    pub e2e_p99: Option<f64>,
}

/// Wiersz przekroju węzeł×serwis: produkcja modelu na konkretnym node w danym
/// serwisie (backend + stabilny `service_key`), z percentylami wydajności.
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelNodeServiceRowWire {
    pub node_id: String,
    pub service_key: String,
    pub backend: String,
    pub model_id: String,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
    pub success_count: i64,
    pub error_count: i64,
    pub error_rate: f64,
    pub ttft_p50: Option<f64>,
    pub ttft_p90: Option<f64>,
    pub ttft_p99: Option<f64>,
    pub decode_p50: Option<f64>,
    pub decode_p90: Option<f64>,
    pub decode_p99: Option<f64>,
}

/// Wiersz cennika per-model (odczyt).
#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub struct ModelPricingWire {
    pub model_id: String,
    pub prompt_per_1k: f64,
    pub completion_per_1k: f64,
    pub audio_per_min: f64,
    pub image_each: f64,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, SerdeSerialize, SerdeDeserialize)]
pub enum ModelMetricsPayload {
    /// `period` ∈ {daily, monthly, hourly} określa granularność `period_key`
    /// (YYYY-MM-DD / YYYY-MM / YYYY-MM-DDTHH), `group_by` ∈ {user, group, model,
    /// node, service, day} wymiar agregacji.
    SummaryRequest {
        period: String,
        period_key: String,
        group_by: String,
        filter: ModelMetricsFilterWire,
    },
    SummaryResponse {
        rows: Vec<ModelMetricsRowWire>,
        /// Unikalna suma (kazdy user policzony RAZ) dla `group_by=group`, gdzie
        /// wiersze grup moga sie nakladac (user w wielu grupach). `None` dla
        /// pozostalych wymiarow, gdzie suma wierszy jest juz rozlaczna.
        grand_total: Option<ModelMetricsRowWire>,
    },
    NodeServiceRequest {
        period: String,
        period_key: String,
    },
    NodeServiceResponse {
        rows: Vec<ModelNodeServiceRowWire>,
    },
    PricingGet,
    PricingList {
        rows: Vec<ModelPricingWire>,
    },
    PricingSet {
        model_id: String,
        prompt_per_1k: f64,
        completion_per_1k: f64,
        audio_per_min: f64,
        image_each: f64,
    },
    PricingSetResult {
        ok: bool,
        /// Komunikat bledu walidacji cennika (NaN/Inf/ujemna wartosc) gdy
        /// `ok=false`; `None` przy sukcesie.
        error: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    #[test]
    fn model_metrics_payload_round_trip() {
        let payload = ModelMetricsPayload::SummaryRequest {
            period: "daily".to_string(),
            period_key: "2026-06-30".to_string(),
            group_by: "model".to_string(),
            filter: ModelMetricsFilterWire {
                model: Some("qwen3-27b".to_string()),
                node: None,
                service: Some("vllm:qwen3".to_string()),
                backend: None,
                modality: Some("chat".to_string()),
            },
        };
        let bytes = crate::cbor::encode(&payload).expect("encode");
        let decoded = crate::cbor::decode::<ModelMetricsPayload>(&bytes).expect("decode");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn message_body_model_metrics_round_trip() {
        let body = MessageBody::ModelMetricsBody(ModelMetricsPayload::SummaryResponse {
            rows: vec![ModelMetricsRowWire {
                key: "qwen3-27b".to_string(),
                prompt_tokens: 1200,
                completion_tokens: 800,
                total_tokens: 2000,
                embedding_tokens: 0,
                audio_ms: 0,
                images: 0,
                request_count: 10,
                success_count: 9,
                error_count: 1,
                cost: 0.42,
                missing_pricing: false,
                error_rate: 0.1,
                ttft_p50: Some(120.0),
                ttft_p90: Some(400.0),
                ttft_p99: Some(1600.0),
                decode_p50: Some(40.0),
                decode_p90: Some(80.0),
                decode_p99: Some(160.0),
                e2e_p50: Some(500.0),
                e2e_p90: Some(2000.0),
                e2e_p99: None,
            }],
            grand_total: Some(ModelMetricsRowWire {
                key: "__grand_total__".to_string(),
                prompt_tokens: 1200,
                completion_tokens: 800,
                total_tokens: 2000,
                embedding_tokens: 0,
                audio_ms: 0,
                images: 0,
                request_count: 10,
                success_count: 9,
                error_count: 1,
                cost: 0.42,
                missing_pricing: true,
                error_rate: 0.1,
                ttft_p50: Some(120.0),
                ttft_p90: Some(400.0),
                ttft_p99: Some(1600.0),
                decode_p50: Some(40.0),
                decode_p90: Some(80.0),
                decode_p99: Some(160.0),
                e2e_p50: Some(500.0),
                e2e_p90: Some(2000.0),
                e2e_p99: None,
            }),
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }
}
