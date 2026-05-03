// =============================================================================
// Plik: metrics/service_stats.rs
// Opis: Statystyki per-serwis: CPU, GPU, pamiec, latencja, status polaczenia.
// =============================================================================

use serde::Serialize;

/// Statystyki pojedynczego serwisu podlaczonego do routera.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceStats {
    /// Nazwa serwisu (np. "bielik-11b-llm")
    pub service_name: String,
    /// Typ serwisu (np. "LLM", "Embedding", "STT", "TTS")
    pub service_type: String,
    /// Status polaczenia: "connected", "disconnected", "error"
    pub status: String,
    /// Uzycie CPU w procentach (0.0 - 100.0)
    pub cpu_usage: f32,
    /// Zuzyta pamiec RAM w MB
    pub memory_mb: u64,
    /// Uzycie GPU w procentach (0.0 - 100.0)
    pub gpu_usage: f32,
    /// Zuzyta pamiec GPU w MB
    pub gpu_memory_mb: u64,
    /// Aktualnie zaladowany model (jesli dotyczy)
    pub loaded_model: Option<String>,
    /// Laczna liczba obsluzonych requestow
    pub requests_total: u64,
    /// Srednia latencja odpowiedzi w milisekundach
    pub avg_latency_ms: u64,
    /// Czas ostatniego health checka (ISO 8601)
    pub last_health_check: Option<String>,
}

impl ServiceStats {
    /// Tworzy nowy wpis statystyk z domyslnymi wartosciami.
    pub fn new(service_name: impl Into<String>, service_type: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            service_type: service_type.into(),
            status: "disconnected".to_string(),
            cpu_usage: 0.0,
            memory_mb: 0,
            gpu_usage: 0.0,
            gpu_memory_mb: 0,
            loaded_model: None,
            requests_total: 0,
            avg_latency_ms: 0,
            last_health_check: None,
        }
    }
}
