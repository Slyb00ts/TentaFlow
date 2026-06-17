// ===== File: ml_studio/train_autogluon.rs — AutoGluon HTTP training engine =====
//
// Drugi silnik treningu tabularnego ML Studio: zamiast trenować w procesie
// (silnik Rust w `train_tabular.rs`), wysyła surowy dataset do zewnętrznego
// serwisu HTTP AutoGluon (Python, port 8201) i odpytuje go o wynik. Zwraca
// dokładnie ten sam `TrainOutcome` co silnik Rust, więc warstwa zapisu i
// budowa odpowiedzi w dispatcherze są wspólne dla obu silników.
//
// HTTP idzie przez `ureq` — czysto synchroniczny klient bez własnego runtime
// tokio. Handler `ml_studio_tabular_train` jest synchroniczny, ale wykonuje się
// na workerze tokio (spawn_blocking); `reqwest::blocking` tworzy tam zagnieżdżony
// runtime i panikuje przy jego niszczeniu ("Cannot drop a runtime in a context
// where blocking is not allowed"), trzymając RPC aż do timeoutu. `ureq` blokuje
// tylko bieżący wątek (akceptowalne — ścieżka Rust też liczy synchronicznie).

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use serde::Deserialize;

use super::train_tabular::{LeaderboardEntry, Task, TrainOutcome};

/// Odstęp między kolejnymi odpytaniami `GET /status/{job_id}`.
const POLL_INTERVAL: Duration = Duration::from_millis(1500);

/// Twardy limit na cały job (od przyjęcia do `succeeded`/`failed`). Po jego
/// przekroczeniu przerywamy polling i zwracamy błąd, żeby handler nie wisiał.
const JOB_TIMEOUT: Duration = Duration::from_secs(600);

/// Budżet czasu treningu przekazywany serwisowi AutoGluon (jego wewnętrzny
/// `time_limit`). Krótszy niż `JOB_TIMEOUT`, bo serwis potrzebuje jeszcze czasu
/// na ewaluację i zbudowanie leaderboardu po wytrenowaniu.
const TIME_LIMIT_SECS: u64 = 180;

#[derive(Deserialize)]
struct TrainResponse {
    job_id: String,
}

#[derive(Deserialize)]
struct StatusResponse {
    status: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    result: Option<ResultPayload>,
}

#[derive(Deserialize)]
struct ResultPayload {
    target_column: String,
    train_rows: usize,
    holdout_rows: usize,
    #[serde(default)]
    class_labels: Vec<String>,
    best_model_name: String,
    #[serde(default)]
    leaderboard: Vec<LeaderboardRow>,
}

#[derive(Deserialize)]
struct LeaderboardRow {
    model_name: String,
    framework: String,
    #[serde(default)]
    accuracy: Option<f64>,
    #[serde(default)]
    f1_macro: Option<f64>,
    #[serde(default)]
    rmse: Option<f64>,
    #[serde(default)]
    r2: Option<f64>,
    #[serde(default)]
    train_secs: f64,
}

fn client() -> ureq::Agent {
    // Pojedyncze żądania (start jobu, polling) są krótkie; trening biegnie
    // asynchronicznie po stronie serwisu, więc czekamy na nie przez polling.
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into()
}

/// Trenuje model tabularny przez serwis AutoGluon. `endpoint_url` to bazowy URL
/// serwisu (`endpoint_url` z wiersza usługi), `raw_data` to surowe bajty
/// datasetu (CSV/XLSX) w oryginalnym formacie, `filename` niesie rozszerzenie,
/// po którym serwis rozpoznaje parser. Zwraca `TrainOutcome` zmapowany z
/// leaderboardu serwisu.
pub fn train_via_service(
    endpoint_url: &str,
    raw_data: &[u8],
    filename: &str,
    target_col: &str,
    task: Task,
) -> Result<TrainOutcome> {
    let base = endpoint_url.trim_end_matches('/');
    let http = client();

    let dataset_b64 = base64::engine::general_purpose::STANDARD.encode(raw_data);
    let train_body = serde_json::json!({
        "dataset_b64": dataset_b64,
        "filename": filename,
        "target_column": target_col,
        "task": task.slug(),
        "time_limit_secs": TIME_LIMIT_SECS,
    });

    let start_url = format!("{}/train_tabular", base);
    let mut resp = http
        .post(&start_url)
        .send_json(&train_body)
        .map_err(|err| map_http_error("POST", &start_url, err))?;
    let job: TrainResponse = resp
        .body_mut()
        .read_json()
        .context("decode train_tabular response")?;

    let status_url = format!("{}/status/{}", base, job.job_id);
    let deadline = Instant::now() + JOB_TIMEOUT;
    loop {
        if Instant::now() >= deadline {
            bail!(
                "AutoGluon job {} timed out after {}s",
                job.job_id,
                JOB_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(POLL_INTERVAL);

        let mut sresp = http
            .get(&status_url)
            .call()
            .map_err(|err| map_http_error("GET", &status_url, err))?;
        let status: StatusResponse = sresp
            .body_mut()
            .read_json()
            .context("decode status response")?;

        match status.status.as_str() {
            "running" => continue,
            "failed" => {
                let msg = status
                    .error
                    .unwrap_or_else(|| "AutoGluon reported failure without detail".to_string());
                bail!("AutoGluon training failed: {}", msg);
            }
            "succeeded" => {
                let result = status
                    .result
                    .context("AutoGluon succeeded but returned no result payload")?;
                return Ok(map_result(task, result));
            }
            other => bail!("AutoGluon returned unknown status '{}'", other),
        }
    }
}

/// Mapuje błąd `ureq` na `anyhow::Error` z czytelnym kontekstem. ureq 3.x dla
/// odpowiedzi spoza 2xx zwraca `Error::StatusCode`, niosąc samą wartość statusu;
/// treść błędu serwisu wyciągamy z dołączonej odpowiedzi gdy jest dostępna.
fn map_http_error(method: &str, url: &str, err: ureq::Error) -> anyhow::Error {
    match err {
        ureq::Error::StatusCode(code) => {
            anyhow::anyhow!("{} {} failed ({})", method, url, code)
        }
        other => anyhow::Error::new(other).context(format!("{} {}", method, url)),
    }
}

/// Mapuje payload serwisu na wspólny `TrainOutcome`. AutoGluon nie zwraca listy
/// cech ani krzywej straty, więc `feature_names`/`best_loss_curve` zostają puste
/// — warstwa zapisu i odpowiedzi to toleruje (puste = brak danych do wykresu).
fn map_result(task: Task, result: ResultPayload) -> TrainOutcome {
    let leaderboard = result
        .leaderboard
        .into_iter()
        .map(|row| LeaderboardEntry {
            model_name: row.model_name,
            framework: row.framework,
            accuracy: row.accuracy,
            f1_macro: row.f1_macro,
            rmse: row.rmse,
            r2: row.r2,
            train_secs: row.train_secs,
        })
        .collect();

    TrainOutcome {
        task,
        target_column: result.target_column,
        feature_names: Vec::new(),
        train_rows: result.train_rows,
        holdout_rows: result.holdout_rows,
        class_labels: result.class_labels,
        leaderboard,
        best_model_name: result.best_model_name,
        best_loss_curve: Vec::new(),
    }
}
