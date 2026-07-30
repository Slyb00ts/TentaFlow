// ===== File: ml_studio/live_view.rs — live-view treningu + statystyki GPU =====
//
// Wspiera podgląd trenowania NA ŻYWO i panel jobów ML Studio. Dwie warstwy:
//  1. Rejestr LOKALNYCH jobów (run_id → base URL serwisu + job_id serwisu),
//     zapełniany przez lokalne pętle treningu (recognition/classifier) tuż po
//     starcie jobu i czyszczony po jego zakończeniu. Pozwala handlerom odpytać
//     serwis `/status/{job_id}` o pola live-view (eta/elapsed/gpu_mem/stage).
//  2. Odczyt statystyk GPU węzła przez `nvidia-smi` (tolerancyjny — brak
//     narzędzia = zera, nigdy błąd).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Pola live-view wyciągnięte z odpowiedzi serwisu treningowego `/status`.
/// Wszystkie tolerancyjne: brak pola w JSON → wartość domyślna (0 / "").
#[derive(Debug, Clone, Default)]
pub struct LiveView {
    pub epoch: i32,
    pub total_epochs: i32,
    pub eta_s: f32,
    pub elapsed_s: f32,
    pub gpu_mem_mb: f32,
    pub stage: String,
}

// Rejestr LOKALNYCH jobów treningowych na tym węźle: run_id → (base, job_id).
// `base` to bazowy URL serwisu (bez końcowego `/`), `job_id` to identyfikator
// jobu zwrócony przez `/train`. Jobów zdalnych (mesh) tu NIE trzymamy.
static LOCAL_JOBS: OnceLock<Mutex<HashMap<String, (String, String)>>> = OnceLock::new();

fn local_jobs() -> &'static Mutex<HashMap<String, (String, String)>> {
    LOCAL_JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Rejestruje lokalny job pod `run_id` (wołane przez pętlę treningu po starcie).
pub fn register_local_job(run_id: &str, base: &str, job_id: &str) {
    if let Ok(mut m) = local_jobs().lock() {
        m.insert(run_id.to_string(), (base.to_string(), job_id.to_string()));
    }
}

/// Usuwa wpis jobu (wołane po zakończeniu treningu — sukces lub błąd).
pub fn clear_local_job(run_id: &str) {
    if let Ok(mut m) = local_jobs().lock() {
        m.remove(run_id);
    }
}

/// Zwraca `(base, job_id)` lokalnego jobu, jeśli zarejestrowany.
pub fn local_job(run_id: &str) -> Option<(String, String)> {
    local_jobs()
        .lock()
        .ok()
        .and_then(|m| m.get(run_id).cloned())
}

// Runy, których pętla nadzoru żyje W TYM procesie. Marker zakłada się przy starcie
// taska treningowego — czyli ZANIM serwis przyjmie job i pojawi się wpis w
// `LOCAL_JOBS` (przygotowanie datasetu potrafi trwać minuty). Dzięki temu brak
// markera jednoznacznie znaczy „ten proces nie nadzoruje tego runu", co pozwala
// odróżnić trening w toku od runu OSIEROCONEGO przez restart Core — bez zgadywania
// po czasie startu.
static LOCAL_RUNS: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();

fn local_runs() -> &'static Mutex<std::collections::HashSet<String>> {
    LOCAL_RUNS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Zgłasza, że pętla nadzoru runu startuje w tym procesie.
pub fn register_local_run(run_id: &str) {
    if let Ok(mut m) = local_runs().lock() {
        m.insert(run_id.to_string());
    }
}

/// Zdejmuje marker po zakończeniu pętli nadzoru (sukces, błąd, anulowanie).
pub fn forget_local_run(run_id: &str) {
    if let Ok(mut m) = local_runs().lock() {
        m.remove(run_id);
    }
}

/// Czy ten proces nadzoruje run (marker albo już zarejestrowany job serwisu).
pub fn is_local_run_live(run_id: &str) -> bool {
    if local_runs()
        .lock()
        .map(|m| m.contains(run_id))
        .unwrap_or(false)
    {
        return true;
    }
    local_job(run_id).is_some()
}

// Runy, dla których użytkownik zażądał anulowania. Rejestr jest niezależny od
// rejestru jobów, bo żądanie może zastać run w KAŻDEJ fazie: pakowania datasetu,
// transferu przez mesh albo już właściwego treningu. Każda z tych pętli sprawdza
// tę flagę i kończy pracę, a serwis treningowy dostaje osobno `POST /cancel`.
static CANCEL_REQUESTS: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();

fn cancel_requests() -> &'static Mutex<std::collections::HashSet<String>> {
    CANCEL_REQUESTS.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// Zapisuje żądanie anulowania runu (pętle treningu/transferu je zobaczą).
pub fn request_cancel(run_id: &str) {
    if let Ok(mut m) = cancel_requests().lock() {
        m.insert(run_id.to_string());
    }
}

/// Czy dla runu zażądano anulowania.
pub fn is_cancel_requested(run_id: &str) -> bool {
    cancel_requests()
        .lock()
        .map(|m| m.contains(run_id))
        .unwrap_or(false)
}

/// Zdejmuje żądanie anulowania (po domknięciu runu — bez tego kolejny run o tym
/// samym id nigdy by nie wystartował; id są UUID, więc to czysto higieniczne).
pub fn clear_cancel(run_id: &str) {
    if let Ok(mut m) = cancel_requests().lock() {
        m.remove(run_id);
    }
}

/// Blokujący `POST {base}/cancel/{job_id}` do serwisu treningowego. Zwraca `true`,
/// gdy serwis przyjął żądanie. Brak endpointu / błąd sieci → `false`: flaga
/// `request_cancel` i tak zatrzyma pętlę nadzoru po stronie Core.
pub fn cancel_service_job_blocking(base: &str, job_id: &str) -> bool {
    let url = format!("{}/cancel/{}", base.trim_end_matches('/'), job_id);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into();
    agent.post(&url).send_empty().is_ok()
}

/// Anuluje LOKALNY job treningowy runu: zapisuje flagę i woła `/cancel` serwisu.
/// Zwraca `true`, gdy serwis potwierdził przyjęcie żądania.
pub async fn cancel_local_job(run_id: &str) -> bool {
    request_cancel(run_id);
    let Some((base, job_id)) = local_job(run_id) else {
        return false;
    };
    tokio::task::spawn_blocking(move || cancel_service_job_blocking(&base, &job_id))
        .await
        .unwrap_or(false)
}

/// Parsuje pola live-view z surowej odpowiedzi `/status` serwisu treningowego.
/// Toleruje brak każdego pola (defaulty). Publiczne, by handlery zdalne (mesh)
/// mogły reużyć tę samą logikę na `status_json` z węzła B.
pub fn parse_live_view(status: &serde_json::Value) -> LiveView {
    let f32_of = |key: &str| status.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
    let i32_of = |key: &str| status.get(key).and_then(|v| v.as_i64()).unwrap_or(0) as i32;
    LiveView {
        epoch: i32_of("epoch"),
        total_epochs: i32_of("total_epochs"),
        eta_s: f32_of("eta_s"),
        elapsed_s: f32_of("elapsed_s"),
        gpu_mem_mb: f32_of("gpu_mem_mb"),
        stage: status
            .get("stage")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    }
}

/// Blokujący GET `{base}/status/{job_id}` → pola live-view. Błąd sieci/parsowania
/// → `LiveView::default()` (podgląd na żywo nigdy nie wywraca handlera).
pub fn fetch_live_view_blocking(base: &str, job_id: &str) -> LiveView {
    let url = format!("{}/status/{}", base.trim_end_matches('/'), job_id);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into();
    match agent.get(&url).call() {
        Ok(mut resp) => match resp.body_mut().read_json::<serde_json::Value>() {
            Ok(v) => parse_live_view(&v),
            Err(_) => LiveView::default(),
        },
        Err(_) => LiveView::default(),
    }
}

/// Live-view dla LOKALNEGO runu: znajduje job w rejestrze i odpytuje serwis.
/// Gdy run nie ma lokalnego jobu (np. zdalny albo już sprzątnięty) → default.
/// Wykonanie GET przenosimy na pulę blokującą, by nie blokować runtime'u async.
pub async fn fetch_local_live_view(run_id: &str) -> LiveView {
    let Some((base, job_id)) = local_job(run_id) else {
        return LiveView::default();
    };
    tokio::task::spawn_blocking(move || fetch_live_view_blocking(&base, &job_id))
        .await
        .unwrap_or_default()
}

/// Statystyki GPU pierwszej karty przez `nvidia-smi`. Gdy narzędzie niedostępne
/// lub odpowiedź nie da się sparsować → wszystkie pola zerowe (bez błędu).
pub fn gpu_stats() -> tentaflow_protocol::GpuStats {
    let zero = tentaflow_protocol::GpuStats {
        name: String::new(),
        mem_used_mb: 0,
        mem_total_mb: 0,
        util_pct: 0,
    };
    let output = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.used,memory.total,utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output();
    let Ok(out) = output else {
        return zero;
    };
    if !out.status.success() {
        return zero;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let Some(first) = stdout.lines().next() else {
        return zero;
    };
    // Format CSV: "NazwaGPU, 1234, 24576, 57"
    let cols: Vec<&str> = first.split(',').map(|c| c.trim()).collect();
    if cols.len() < 4 {
        return zero;
    }
    tentaflow_protocol::GpuStats {
        name: cols[0].to_string(),
        mem_used_mb: cols[1].parse().unwrap_or(0),
        mem_total_mb: cols[2].parse().unwrap_or(0),
        util_pct: cols[3].parse().unwrap_or(0),
    }
}
