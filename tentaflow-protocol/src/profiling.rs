// =============================================================================
// Plik: profiling.rs
// Opis: Typy protokolu dla profilowania NVIDIA Nsight Systems — sesje (start /
//       stop / list / report / delete) oraz raport (ProfileReport) z metadanymi,
//       KPI, top tabelami i timeline'em wykorzystania GPU. rkyv zero-copy.
// =============================================================================

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

// =============================================================================
// Zakres profilowania i status sesji
// =============================================================================

/// Zakres zbierania danych profilera.
/// Sterowane z GUI; mapuje sie na flagi `nsys profile --trace=...`.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub enum NsightScope {
    /// Tylko CPU (sampling + osapi).
    Cpu,
    /// Pojedynczy GPU po indeksie (CUDA dla wskazanego device).
    GpuIndex(u8),
    /// Wszystkie widoczne GPU.
    GpuAll,
    /// CPU + jeden konkretny GPU (najbardziej uzyteczne dla diag pojedynczego serwisu).
    BothIndex(u8),
    /// CPU + wszystkie GPU.
    BothAll,
}

/// Stan zycia sesji profilowania na nodzie.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub enum NsightSessionStatus {
    /// Trwa zbieranie danych (`nsys profile` w trakcie).
    Running,
    /// Wyslano `nsys stop`, czekamy na zamkniecie pliku `.nsys-rep`.
    Stopping,
    /// Zakonczono, raport mozliwy do przeczytania.
    Done,
    /// Niepowodzenie — szczegol w polu `error` rekordu.
    Failed,
}

// =============================================================================
// Cel GPU i wpis sesji
// =============================================================================

/// Pojedynczy GPU wybierany jako cel profilowania.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightGpuTarget {
    /// Indeks GPU widoczny dla CUDA / nvidia-smi.
    pub idx: u8,
    /// Czytelna nazwa modelu GPU (np. "NVIDIA RTX 4090").
    pub name: String,
}

/// Rekord sesji w katalogu nodu.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct NsightSessionEntry {
    /// Identyfikator sesji (UUID lub timestamp-based slug).
    pub session_id: String,
    /// User-friendly etykieta (np. "vllm-cold-start").
    pub label: String,
    /// Zakres zbierania danych.
    pub scope: NsightScope,
    /// Status zycia sesji.
    pub status: NsightSessionStatus,
    /// Moment startu (unix epoch ms).
    pub started_at_ms: u64,
    /// Czas trwania sesji w ms (0 dopoki Running).
    pub duration_ms: u64,
    /// Rozmiar `.nsys-rep` w bajtach (0 dopoki nie zamkniety).
    pub size_bytes: u64,
    /// Komunikat bledu — wypelniany dla `Failed`.
    pub error: Option<String>,
}

// =============================================================================
// Pary request/response — sterowanie sesjami
// =============================================================================

/// Start nowej sesji profilowania.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct NsightStartRequest {
    /// Nod docelowy.
    pub node_id: String,
    /// Zakres profilowania.
    pub scope: NsightScope,
    /// Maksymalny czas trwania (s) — auto-stop po wygasnieciu.
    pub duration_secs: u32,
    /// User-friendly etykieta sesji.
    pub label: String,
}

/// Potwierdzenie startu z `session_id`.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightStartResponse {
    pub session_id: String,
    pub started_at_ms: u64,
}

/// Wczesniejsze zatrzymanie biezacej sesji.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightStopRequest {
    pub node_id: String,
    pub session_id: String,
}

/// Status sesji po wyslaniu stop.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightStopResponse {
    pub session_id: String,
    pub status: NsightSessionStatus,
}

/// Lista wszystkich sesji widocznych na nodzie.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightSessionsRequest {
    pub node_id: String,
}

/// Odpowiedz z lista sesji.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct NsightSessionsResponse {
    pub node_id: String,
    pub sessions: Vec<NsightSessionEntry>,
}

/// Pobranie sparsowanego raportu (`.nsys-rep` -> JSON via `nsys stats`).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightReportRequest {
    pub node_id: String,
    pub session_id: String,
}

/// Odpowiedz z pelnym raportem — meta + KPI + top tabele + timeline.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct NsightReportResponse {
    pub report: ProfileReport,
}

/// Usuniecie zapisanego raportu i metadanych sesji.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightDeleteRequest {
    pub node_id: String,
    pub session_id: String,
}

/// Potwierdzenie usuniecia.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightDeleteResponse {
    pub session_id: String,
    pub ok: bool,
}

/// Request pobrania surowego pliku `.nsys-rep` (do otwarcia w nsys-ui).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightDownloadRequest {
    pub node_id: String,
    pub session_id: String,
}

/// Odpowiedz: cala zawartosc pliku `.nsys-rep` jako jeden binary blob.
/// `bytes` ma rzad 1-50 MB; rkyv pakuje to w pojedynczy alloc dla Vec<u8>.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq, Eq)]
pub struct NsightDownloadResponse {
    pub session_id: String,
    /// Sugerowana nazwa pliku do zapisu po stronie klienta.
    pub filename: String,
    pub bytes: Vec<u8>,
}

// =============================================================================
// Raport profilowania (ProfileReport + sub-struktury)
// =============================================================================

/// Metadane przebiegu — co, gdzie, kiedy, na czym.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct ProfileMeta {
    pub session_id: String,
    pub label: String,
    pub scope: NsightScope,
    pub hostname: String,
    pub started_at_ms: u64,
    pub duration_ms: u64,
    /// Wersja `nsys` ktora zebrala dane (do diag kompatybilnosci).
    pub nsys_version: String,
    /// Lista GPU objetych sesja (puste dla Cpu).
    pub gpu_targets: Vec<NsightGpuTarget>,
}

/// Zagregowane wskazniki przebiegu — pokazywane jako kafelki na dashboardzie.
/// Brak `Eq` przez floaty (NaN ≠ NaN).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct ProfileKpi {
    pub total_gpu_active_ms: f64,
    pub total_cpu_active_ms: f64,
    pub kernel_count: u64,
    pub cuda_api_count: u64,
    pub peak_vram_mb: u64,
    pub samples_collected: u64,
}

impl Default for ProfileKpi {
    fn default() -> Self {
        Self {
            total_gpu_active_ms: 0.0,
            total_cpu_active_ms: 0.0,
            kernel_count: 0,
            cuda_api_count: 0,
            peak_vram_mb: 0,
            samples_collected: 0,
        }
    }
}

/// Wiersz tabeli top-N (kernel, CUDA API, mem op, CPU sample, NVTX range).
/// Brak `Eq` przez f64/f32.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct ProfileTopRow {
    /// Nazwa elementu (mangled symbol, CUDA API, NVTX label, ...).
    pub name: String,
    /// Sumaryczny czas w ms.
    pub total_ms: f64,
    /// Liczba wywolan / probek.
    pub calls: u64,
    /// Sredni czas pojedynczego wywolania w ms.
    pub avg_ms: f64,
    /// Udzial procentowy w bucket'cie (0.0 - 100.0).
    pub pct: f32,
}

/// Pojedyncza probka utylizacji GPU w timeline (sampling co stala wartosc ms).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct GpuUtilSample {
    /// Czas od poczatku sesji w ms.
    pub t_ms: u32,
    /// Wykorzystanie SM (0-100).
    pub sm_pct: u8,
    /// Wykorzystanie pamieci (0-100).
    pub mem_pct: u8,
    /// VRAM uzyte w MB.
    pub vram_used_mb: u32,
    /// Pobor mocy w watach.
    pub power_w: f32,
}

/// Timeline pojedynczego GPU — limit mocy + lista probek.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct GpuUtilSeries {
    pub gpu_idx: u8,
    pub power_limit_w: f32,
    pub samples: Vec<GpuUtilSample>,
}

/// Pelny raport sesji — agregat zwracany w `NsightReportResponse`.
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub struct ProfileReport {
    pub meta: ProfileMeta,
    pub kpi: ProfileKpi,
    /// Top kernele GPU wg czasu (zwykle limit 50).
    pub gpu_kernels_top: Vec<ProfileTopRow>,
    /// Top wywolania CUDA Runtime API wg czasu.
    pub cuda_api_top: Vec<ProfileTopRow>,
    /// Top operacje pamieciowe GPU (memcpy, memset).
    pub gpu_mem_ops: Vec<ProfileTopRow>,
    /// Top probki CPU sampling (po symbolu).
    pub cpu_samples_top: Vec<ProfileTopRow>,
    /// Top zakresy NVTX (jesli aplikacja je emituje).
    pub nvtx_ranges_top: Vec<ProfileTopRow>,
    /// Timeline utylizacji per GPU.
    pub gpu_util_timeline: Vec<GpuUtilSeries>,
}

// =============================================================================
// Inner-enum pack — jeden slot w MessageBody (limit 256 wariantow rkyv).
// =============================================================================

/// Wszystkie request/response Nsight w jednym enumie. Trzymane jako jeden
/// wariant `MessageBody::NsightBody(NsightPayload)`, zeby zaoszczedzic 9 slotow
/// w MessageBody (rkyv ma twardy limit 256 wariantow w enumie).
#[derive(Archive, Deserialize, Serialize, SerdeSerialize, SerdeDeserialize, Debug, Clone, PartialEq)]
pub enum NsightPayload {
    StartRequest(NsightStartRequest),
    StartResponse(NsightStartResponse),
    StopRequest(NsightStopRequest),
    StopResponse(NsightStopResponse),
    SessionsRequest(NsightSessionsRequest),
    SessionsResponse(NsightSessionsResponse),
    ReportRequest(NsightReportRequest),
    ReportResponse(NsightReportResponse),
    DeleteRequest(NsightDeleteRequest),
    DeleteResponse(NsightDeleteResponse),
    DownloadRequest(NsightDownloadRequest),
    DownloadResponse(NsightDownloadResponse),
}

// =============================================================================
// Testy round-trip rkyv
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! round_trip {
        ($ty:ty, $value:expr) => {{
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&$value).expect("encode");
            rkyv::from_bytes::<$ty, rkyv::rancor::Error>(&bytes).expect("decode")
        }};
    }

    #[test]
    fn nsight_start_request_round_trip() {
        let req = NsightStartRequest {
            node_id: "node-alpha".to_string(),
            scope: NsightScope::BothIndex(0),
            duration_secs: 60,
            label: "vllm-cold-start".to_string(),
        };
        assert_eq!(round_trip!(NsightStartRequest, req.clone()), req);
    }

    #[test]
    fn nsight_report_response_large_round_trip() {
        // Konstrukcja duzego raportu: 50 kerneli, 30 CUDA API, 3 timeline po 600 probek.
        let kernels: Vec<ProfileTopRow> = (0..50)
            .map(|i| ProfileTopRow {
                name: format!("ampere_sgemm_{}x{}_nn", i, i + 1),
                total_ms: (i as f64) * 1.5,
                calls: (i as u64) * 7 + 1,
                avg_ms: 0.123 + i as f64 * 0.01,
                pct: i as f32 / 50.0,
            })
            .collect();

        let cuda_api: Vec<ProfileTopRow> = (0..30)
            .map(|i| ProfileTopRow {
                name: format!("cudaApi_{}", i),
                total_ms: (i as f64) * 0.7,
                calls: i as u64 + 10,
                avg_ms: 0.05,
                pct: i as f32 / 30.0,
            })
            .collect();

        let series: Vec<GpuUtilSeries> = (0..3u8)
            .map(|gpu_idx| GpuUtilSeries {
                gpu_idx,
                power_limit_w: 450.0,
                samples: (0..600)
                    .map(|t| GpuUtilSample {
                        t_ms: t * 50,
                        sm_pct: ((t + gpu_idx as u32) % 101) as u8,
                        mem_pct: ((t * 2) % 101) as u8,
                        vram_used_mb: 1000 + t,
                        power_w: 100.0 + (t as f32 % 350.0),
                    })
                    .collect(),
            })
            .collect();

        let report = ProfileReport {
            meta: ProfileMeta {
                session_id: "sess-001".to_string(),
                label: "stress".to_string(),
                scope: NsightScope::BothAll,
                hostname: "spark-001".to_string(),
                started_at_ms: 1_710_000_000_000,
                duration_ms: 30_000,
                nsys_version: "2024.5.1".to_string(),
                gpu_targets: vec![
                    NsightGpuTarget {
                        idx: 0,
                        name: "NVIDIA RTX 4090".to_string(),
                    },
                    NsightGpuTarget {
                        idx: 1,
                        name: "NVIDIA RTX 4090".to_string(),
                    },
                ],
            },
            kpi: ProfileKpi {
                total_gpu_active_ms: 28_400.5,
                total_cpu_active_ms: 12_000.25,
                kernel_count: 1_234_567,
                cuda_api_count: 9_876_543,
                peak_vram_mb: 23_500,
                samples_collected: 1800,
            },
            gpu_kernels_top: kernels,
            cuda_api_top: cuda_api,
            gpu_mem_ops: vec![ProfileTopRow {
                name: "[CUDA memcpy HtoD]".to_string(),
                total_ms: 42.0,
                calls: 100,
                avg_ms: 0.42,
                pct: 100.0,
            }],
            cpu_samples_top: Vec::new(),
            nvtx_ranges_top: Vec::new(),
            gpu_util_timeline: series,
        };

        let response = NsightReportResponse { report };
        let decoded = round_trip!(NsightReportResponse, response.clone());
        assert_eq!(decoded, response);
        assert_eq!(decoded.report.gpu_kernels_top.len(), 50);
        assert_eq!(decoded.report.cuda_api_top.len(), 30);
        assert_eq!(decoded.report.gpu_util_timeline.len(), 3);
        for s in &decoded.report.gpu_util_timeline {
            assert_eq!(s.samples.len(), 600);
        }
    }

    #[test]
    fn nsight_scope_variants_serialize() {
        // Wszystkie 5 wariantow przechodzi round-trip i da sie odroznic.
        let variants = [
            NsightScope::Cpu,
            NsightScope::GpuIndex(3),
            NsightScope::GpuAll,
            NsightScope::BothIndex(1),
            NsightScope::BothAll,
        ];
        for v in &variants {
            let decoded = round_trip!(NsightScope, v.clone());
            assert_eq!(&decoded, v);
        }
    }

    #[test]
    fn nsight_download_response_large_blob_round_trip() {
        // Smoke: 1 MB binary blob musi przejsc rkyv encode/decode bez utraty.
        let bytes: Vec<u8> = (0..(1024 * 1024)).map(|i| (i % 251) as u8).collect();
        let resp = NsightDownloadResponse {
            session_id: "sess-binary".to_string(),
            filename: "nsight-sess-binary.nsys-rep".to_string(),
            bytes: bytes.clone(),
        };
        let payload = NsightPayload::DownloadResponse(resp.clone());
        let decoded = round_trip!(NsightPayload, payload.clone());
        match decoded {
            NsightPayload::DownloadResponse(d) => {
                assert_eq!(d.session_id, resp.session_id);
                assert_eq!(d.filename, resp.filename);
                assert_eq!(d.bytes.len(), bytes.len());
                assert_eq!(d.bytes, bytes);
            }
            _ => panic!("oczekiwano DownloadResponse"),
        }
    }

    #[test]
    fn nsight_payload_inner_enum_round_trip() {
        // Wrapper enum musi przeniesc kazdy wariant bez zmiany.
        let payloads = vec![
            NsightPayload::StartRequest(NsightStartRequest {
                node_id: "n".into(),
                scope: NsightScope::Cpu,
                duration_secs: 10,
                label: "x".into(),
            }),
            NsightPayload::DeleteResponse(NsightDeleteResponse {
                session_id: "s".into(),
                ok: true,
            }),
        ];
        for p in &payloads {
            let decoded = round_trip!(NsightPayload, p.clone());
            assert_eq!(&decoded, p);
        }
    }
}
