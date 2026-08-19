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
    /// Only rows of this `user_id`.
    #[serde(default)]
    pub user: Option<String>,
    /// Only rows of users who are members of this group (`user_groups.id`).
    #[serde(default)]
    pub group: Option<String>,
}

/// One aggregated summary row. `key` depends on `group_by` (user/group/
/// model/node/service/day/hour). Percentiles are computed from the SUMMED
/// histograms of all rows in the group; `None` = no samples in that histogram.
/// `display_name`/`subtitle`/`member_count`/`last_seen_at` are entity names
/// resolved Core-side (D3) — `None` when the dimension has none.
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
    /// user → display_name|username|email; group → name; node →
    /// `sync_nodes.display_name` (local node → hostname); model → catalog name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// Second line: user → email|username; other dimensions → `None`.
    #[serde(default)]
    pub subtitle: Option<String>,
    /// Group member count (only `group_by=group`).
    #[serde(default)]
    pub member_count: Option<i64>,
    /// Node last seen (RFC3339, only `group_by=node`); the local node always
    /// reports the current time.
    #[serde(default)]
    pub last_seen_at: Option<String>,
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
    #[serde(default)]
    pub node_display_name: Option<String>,
    #[serde(default)]
    pub node_last_seen_at: Option<String>,
    #[serde(default)]
    pub model_display_name: Option<String>,
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
    /// `period` ∈ {daily, monthly, hourly} sets the `period_key` granularity
    /// (YYYY-MM-DD / YYYY-MM / YYYY-MM-DDTHH), `group_by` ∈ {user, group, model,
    /// node, service, day, hour} the aggregation dimension. `group` rows are keyed by
    /// group id (`user_groups.id`), `hour` by the full `hour_bucket`.
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
                user: Some("00000000-0000-4000-8000-000000000002".to_string()),
                group: None,
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
                display_name: Some("Qwen 3.8 27B".to_string()),
                subtitle: None,
                member_count: None,
                last_seen_at: None,
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
                display_name: None,
                subtitle: None,
                member_count: None,
                last_seen_at: None,
            }),
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    /// A row encoded WITHOUT the new fields (old peer) must decode to the
    /// defaults (`None`), not fail.
    #[test]
    fn summary_row_legacy_cbor_decodes_with_defaults() {
        #[derive(serde::Serialize)]
        struct LegacyRow {
            key: String,
            prompt_tokens: i64,
            completion_tokens: i64,
            total_tokens: i64,
            embedding_tokens: i64,
            audio_ms: i64,
            images: i64,
            request_count: i64,
            success_count: i64,
            error_count: i64,
            cost: f64,
            missing_pricing: bool,
            error_rate: f64,
            ttft_p50: Option<f64>,
            ttft_p90: Option<f64>,
            ttft_p99: Option<f64>,
            decode_p50: Option<f64>,
            decode_p90: Option<f64>,
            decode_p99: Option<f64>,
            e2e_p50: Option<f64>,
            e2e_p90: Option<f64>,
            e2e_p99: Option<f64>,
        }
        let legacy = LegacyRow {
            key: "u1".to_string(),
            prompt_tokens: 1,
            completion_tokens: 2,
            total_tokens: 3,
            embedding_tokens: 0,
            audio_ms: 0,
            images: 0,
            request_count: 1,
            success_count: 1,
            error_count: 0,
            cost: 0.0,
            missing_pricing: false,
            error_rate: 0.0,
            ttft_p50: None,
            ttft_p90: None,
            ttft_p99: None,
            decode_p50: None,
            decode_p90: None,
            decode_p99: None,
            e2e_p50: None,
            e2e_p90: None,
            e2e_p99: None,
        };
        let bytes = crate::cbor::encode(&legacy).expect("encode legacy");
        let decoded = crate::cbor::decode::<ModelMetricsRowWire>(&bytes).expect("decode legacy");
        assert_eq!(decoded.key, "u1");
        assert_eq!(decoded.total_tokens, 3);
        assert_eq!(decoded.display_name, None);
        assert_eq!(decoded.subtitle, None);
        assert_eq!(decoded.member_count, None);
        assert_eq!(decoded.last_seen_at, None);

        #[derive(serde::Serialize)]
        struct LegacyFilter {
            model: Option<String>,
            node: Option<String>,
            service: Option<String>,
            backend: Option<String>,
            modality: Option<String>,
        }
        let bytes = crate::cbor::encode(&LegacyFilter {
            model: Some("m".to_string()),
            node: None,
            service: None,
            backend: None,
            modality: None,
        })
        .expect("encode legacy filter");
        let filter = crate::cbor::decode::<ModelMetricsFilterWire>(&bytes).expect("decode filter");
        assert_eq!(filter.model.as_deref(), Some("m"));
        assert_eq!(filter.user, None);
        assert_eq!(filter.group, None);
    }

    #[test]
    fn node_service_row_round_trip_with_names() {
        let row = ModelNodeServiceRowWire {
            node_id: "d91a".to_string(),
            service_key: "vllm:qwen".to_string(),
            backend: "vllm".to_string(),
            model_id: "qwen".to_string(),
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 2,
            request_count: 1,
            success_count: 1,
            error_count: 0,
            error_rate: 0.0,
            ttft_p50: None,
            ttft_p90: None,
            ttft_p99: None,
            decode_p50: None,
            decode_p90: None,
            decode_p99: None,
            node_display_name: Some("hazai".to_string()),
            node_last_seen_at: Some("2026-08-19T11:00:00Z".to_string()),
            model_display_name: Some("Qwen".to_string()),
        };
        let bytes = crate::cbor::encode(&row).expect("encode");
        let decoded = crate::cbor::decode::<ModelNodeServiceRowWire>(&bytes).expect("decode");
        assert_eq!(decoded, row);
    }
}
