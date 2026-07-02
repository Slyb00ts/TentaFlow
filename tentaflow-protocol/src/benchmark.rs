// =============================================================================
// Plik: benchmark.rs
// Opis: Binarny protokół CBOR dla Benchmark Studio — definicje benchmarków,
//       targety, uruchamianie runów, historia i wyniki oraz live progres runu.
//       Sekrety (api_key) NIGDY nie wracają w wire — target niesie tylko flagę
//       `has_key`; klient wysyła klucz wyłącznie przy zapisie.
// Przykład: MessageBody::BenchmarkBody(BenchmarkPayload::StartRunRequest { .. })
// =============================================================================

use serde::{Deserialize, Serialize};

/// Krótkie podsumowanie runu — do list historii i ostatniego runu benchmarku.
/// `benchmark_name` wypełniane tylko dla listy „ostatnie runy" (RecentRuns),
/// gdzie wiersze pochodzą z wielu benchmarków.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunSummaryWire {
    pub id: String,
    pub benchmark_id: String,
    pub benchmark_name: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub error: Option<String>,
}

/// Wiersz przeglądu benchmarków: definicja + liczba targetów, liczba włączonych
/// scenariuszy (`test_count`) oraz podsumowanie ostatniego runu.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkSummaryWire {
    pub id: String,
    pub name: String,
    pub target_count: u32,
    pub test_count: u32,
    pub last_run: Option<RunSummaryWire>,
}

/// Target w odczycie (edycja/podgląd). Klucz API nigdy nie wraca — tylko flaga
/// `has_key` mówiąca, czy w bazie jest zapisany sekret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetWire {
    pub id: String,
    pub kind: String,
    pub service_ref: Option<String>,
    pub api_type: String,
    pub host: String,
    pub port: u16,
    pub has_key: bool,
    pub model: String,
    pub label: String,
}

/// Pełna definicja benchmarku do edytora: config + targety (bez sekretów).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkWire {
    pub id: String,
    pub name: String,
    pub config_json: String,
    pub targets: Vec<TargetWire>,
    pub created_at: String,
    pub updated_at: String,
}

/// Target przy zapisie. `api_key` obecny WYŁĄCZNIE gdy użytkownik wpisał nowy
/// sekret: `None` zachowuje zapisany klucz, `Some("")` czyści, `Some(k)` zapisuje.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TargetInputWire {
    pub id: String,
    pub kind: String,
    pub service_ref: Option<String>,
    pub api_type: String,
    pub host: String,
    pub port: u16,
    pub api_key: Option<String>,
    pub model: String,
    pub label: String,
}

/// Zagregowany wiersz wyniku runu. Metryki są `Option`, bo backend bez `usage`
/// nie raportuje throughputu, a scenariusz bez udanej próbki nie ma percentyli.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResultRowWire {
    pub target_id: String,
    pub target_label: String,
    pub scenario: String,
    pub variant_json: String,
    pub ttft_ms_mean: Option<f64>,
    pub ttft_ms_sigma: Option<f64>,
    pub prefill_tps_mean: Option<f64>,
    pub prefill_tps_sigma: Option<f64>,
    pub decode_tps_mean: Option<f64>,
    pub decode_tps_sigma: Option<f64>,
    pub total_ms_mean: Option<f64>,
    pub total_ms_sigma: Option<f64>,
    pub p50_ms: Option<f64>,
    pub p90_ms: Option<f64>,
    pub p99_ms: Option<f64>,
    pub requests: u32,
    pub errors: u32,
    pub samples_json: String,
}

/// Rodzina wiadomości Benchmark Studio (request + response). ciborium koduje
/// warianty external-tagged po NAZWIE wariantu, więc nie zmieniaj nazw wariantów
/// ani pól bez aktualizacji frontu i golden testu (`benchmark_wire_golden`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BenchmarkPayload {
    ListRequest,
    ListResponse {
        benchmarks: Vec<BenchmarkSummaryWire>,
    },
    GetRequest {
        id: String,
    },
    GetResponse {
        benchmark: BenchmarkWire,
    },
    SaveRequest {
        /// `None` = nowy benchmark (Core wygeneruje id).
        id: Option<String>,
        name: String,
        config_json: String,
        targets: Vec<TargetInputWire>,
    },
    SaveResponse {
        id: String,
    },
    DeleteRequest {
        id: String,
    },
    DeleteResult {
        ok: bool,
    },
    StartRunRequest {
        benchmark_id: String,
    },
    StartRunResponse {
        run_id: String,
    },
    RunStatusRequest {
        run_id: String,
    },
    RunStatusResponse {
        run_id: String,
        status: String,
        error: Option<String>,
        started_at: String,
        finished_at: Option<String>,
    },
    RunResultsRequest {
        run_id: String,
    },
    RunResultsResponse {
        results: Vec<ResultRowWire>,
    },
    ListRunsRequest {
        benchmark_id: String,
    },
    ListRunsResponse {
        runs: Vec<RunSummaryWire>,
    },
    RecentRunsRequest,
    RecentRunsResponse {
        runs: Vec<RunSummaryWire>,
    },
    CancelRunRequest {
        run_id: String,
    },
    CancelRunResult {
        ok: bool,
    },
    /// Subskrypcja live progresu runu (streaming). `run_id` pełni rolę klucza
    /// szyny logów, analogicznie do `deploy_id` w DeploymentLogStream.
    RunStreamRequest {
        run_id: String,
    },
    RunStreamChunk {
        run_id: String,
        /// "log" | "phase" | "progress" | "result".
        kind: String,
        phase: String,
        line: String,
        progress_pct: u32,
        ts_ms: i64,
    },
    RunStreamEnd {
        run_id: String,
        /// Terminalny status runu: 'success' | 'failed' | 'cancelled'.
        status: String,
        error: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    #[test]
    fn benchmark_payload_round_trip() {
        let payload = BenchmarkPayload::SaveRequest {
            id: None,
            name: "vLLM sweep".to_string(),
            config_json: "{}".to_string(),
            targets: vec![TargetInputWire {
                id: "t1".to_string(),
                kind: "external".to_string(),
                service_ref: None,
                api_type: "openai".to_string(),
                host: "https://api.example.com".to_string(),
                port: 443,
                api_key: Some("secret".to_string()),
                model: "gpt-4o".to_string(),
                label: "OpenAI".to_string(),
            }],
        };
        let bytes = crate::cbor::encode(&payload).expect("encode");
        let decoded = crate::cbor::decode::<BenchmarkPayload>(&bytes).expect("decode");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn message_body_benchmark_round_trip() {
        let body = MessageBody::BenchmarkBody(BenchmarkPayload::RunResultsResponse {
            results: vec![ResultRowWire {
                target_id: "t1".to_string(),
                target_label: "OpenAI".to_string(),
                scenario: "latency".to_string(),
                variant_json: "{}".to_string(),
                ttft_ms_mean: Some(120.0),
                ttft_ms_sigma: Some(10.0),
                prefill_tps_mean: None,
                prefill_tps_sigma: None,
                decode_tps_mean: Some(45.0),
                decode_tps_sigma: Some(2.0),
                total_ms_mean: Some(900.0),
                total_ms_sigma: Some(50.0),
                p50_ms: Some(880.0),
                p90_ms: Some(1200.0),
                p99_ms: None,
                requests: 5,
                errors: 0,
                samples_json: "[]".to_string(),
            }],
        });
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        assert_eq!(decoded, body);
    }

    /// Golden wire snapshot: ciborium koduje warianty enuma jako mapę 1-elementową
    /// z kluczem = NAZWA wariantu (external tagging). Ten test przybija dokładne
    /// bajty, więc przypadkowa zmiana nazwy wariantu, nazwy pola albo tagu
    /// `MessageBody::BenchmarkBody` zostanie wykryta jako regresja wire.
    #[test]
    fn benchmark_wire_golden() {
        // BenchmarkPayload::StartRunRequest { benchmark_id: "b1" }
        let start = BenchmarkPayload::StartRunRequest {
            benchmark_id: "b1".to_string(),
        };
        let bytes = crate::cbor::encode(&start).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes("a16f537461727452756e52657175657374a16c62656e63686d61726b5f6964626231"),
            "StartRunRequest wire drift"
        );

        // MessageBody::BenchmarkBody(StartRunRequest) — zewnętrzny tag body + tag wariantu.
        let body = MessageBody::BenchmarkBody(start);
        let bytes = crate::cbor::encode(&body).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a16d42656e63686d61726b426f6479a16f537461727452756e52657175657374a16c62656e63686d61726b5f6964626231"
            ),
            "MessageBody::BenchmarkBody wire drift"
        );

        // BenchmarkPayload::RunStreamChunk — pełny zestaw pól (kolejność/nazwy).
        let chunk = BenchmarkPayload::RunStreamChunk {
            run_id: "r1".to_string(),
            kind: "log".to_string(),
            phase: String::new(),
            line: "x".to_string(),
            progress_pct: 0,
            ts_ms: 0,
        };
        let bytes = crate::cbor::encode(&chunk).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a16e52756e53747265616d4368756e6ba66672756e5f6964627231646b696e64636c6f6765706861736560646c696e6561786c70726f67726573735f706374006574735f6d7300"
            ),
            "RunStreamChunk wire drift"
        );
    }

    fn hex_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
            .collect()
    }
}
