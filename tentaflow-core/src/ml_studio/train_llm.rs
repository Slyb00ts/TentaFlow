// ===== File: ml_studio/train_llm.rs — async LLM fine-tuning background task =====
//
// Asynchroniczny silnik fine-tuningu LLM ML Studio. W odróżnieniu od treningu
// tabularnego (`train_autogluon.rs`, synchroniczny — trwa sekundy), trening LLM
// trwa MINUTY, więc NIE może blokować RPC. Handler tworzy run w stanie
// `running`, wywołuje `spawn_ft_training` i wraca natychmiast; cała robota
// (start jobu w serwisie ml-training, polling, zapis metryk i modelu) dzieje
// się w `tokio::spawn`. UI odpytuje postęp przez `MlStudioFtTrainStatusRequest`.
//
// Źródła danych w tasku: task NIE ma `HandlerContext`, więc service discovery
// idzie przez `crate::db::global_pool()` (nie `ctx.state.db`), a dane ML Studio
// przez `repository::*` (własny pool przez `global_pool`-niezależny `db::pool()`).
//
// HTTP: `ureq` jest klientem BLOKUJĄCYM — wołanie go wprost w async tasku
// zablokowałoby worker tokio, więc każde żądanie (POST /train, GET /status) jest
// opakowane w `tokio::task::spawn_blocking`. Między pollami `tokio::time::sleep`.

use std::time::Duration;

use serde_json::json;

use crate::ml_studio::repository;
use crate::services_repo;
use tentaflow_protocol::MlStudioFtHyperparams;

/// Odstęp między kolejnymi odpytaniami `GET /status/{job_id}` serwisu.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Twardy limit całego treningu. Po przekroczeniu task przerywa polling i
/// oznacza run jako `failed`, żeby „wiszący" job w serwisie nie został na zawsze
/// w stanie `running` w naszej bazie.
const JOB_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Timeout pojedynczego żądania HTTP (start jobu / pojedynczy status).
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

/// Startuje fine-tuning LLM w tle. Run o `run_id` musi już istnieć w stanie
/// `running` (tworzy go handler). Funkcja zwraca natychmiast — błędy treningu
/// lądują w statusie runu (`failed` + metryka/komunikat), nie są propagowane.
#[allow(clippy::too_many_arguments)]
pub fn spawn_ft_training(
    run_id: String,
    project_id: String,
    owner_user_id: String,
    dataset_id: String,
    base_model: String,
    method: String,
    objective: String,
    teacher_model: Option<String>,
    hyperparams: MlStudioFtHyperparams,
    merge_adapter: bool,
) {
    tokio::spawn(async move {
        if let Err(err) = run_training(
            &run_id,
            &project_id,
            &owner_user_id,
            &dataset_id,
            &base_model,
            &method,
            &objective,
            teacher_model.as_deref(),
            &hyperparams,
            merge_adapter,
        )
        .await
        {
            // Każdy błąd ścieżki treningu kończy run jako `failed`. Zapisujemy
            // też komunikat jako metrykę-tekst nie pasuje (metryki są f64), więc
            // status zawiera tylko stan; szczegół błędu trafia do logu.
            tracing::warn!(run_id = %run_id, error = %err, "LLM fine-tuning failed");
            let _ = repository::update_training_run_status(&run_id, "failed");
        }
    });
}

/// Reprezentacja jednego rekordu treningowego po sparsowaniu datasetu. Serwis
/// ml-training przyjmuje listę dictów; budujemy je jako `serde_json::Value`.
type Record = serde_json::Value;

#[allow(clippy::too_many_arguments)]
async fn run_training(
    run_id: &str,
    project_id: &str,
    owner_user_id: &str,
    dataset_id: &str,
    base_model: &str,
    method: &str,
    objective: &str,
    teacher_model: Option<&str>,
    hyperparams: &MlStudioFtHyperparams,
    merge_adapter: bool,
) -> anyhow::Result<()> {
    // 1. Service discovery: serwis ml-training z kategorii "training". Pool CORE
    //    przez `global_pool` (task nie ma `ctx.state.db`).
    let endpoint = resolve_endpoint()?;

    // 2. Dane: surowe bajty datasetu + jego `kind` (format), zamienione na listę
    //    rekordów dla serwisu. Split na train/eval ~10%.
    let raw = repository::get_dataset_raw(owner_user_id, dataset_id)?;
    let dataset = repository::get_dataset(owner_user_id, dataset_id)?
        .ok_or_else(|| anyhow::anyhow!("dataset not found"))?;
    let records = parse_records(&raw, &dataset.kind)?;
    if records.is_empty() {
        anyhow::bail!("dataset produced no usable records");
    }
    let (train_data, eval_data) = split_eval(records);

    // 3. POST /train w spawn_blocking (ureq jest blocking). Zwraca job_id serwisu.
    let output_dir = format!("ml_studio/{}/{}", project_id, run_id);
    let train_body = json!({
        "base_model": base_model,
        "train_data": train_data,
        "eval_data": eval_data,
        "method": method,
        "objective": objective,
        "teacher_model": teacher_model,
        "hyperparams": {
            "epochs": hyperparams.epochs,
            "lr": hyperparams.learning_rate,
            "batch_size": hyperparams.batch_size,
            "grad_accum": hyperparams.grad_accum_steps,
            "lora_r": hyperparams.lora_r,
            "lora_alpha": hyperparams.lora_alpha,
            "lora_dropout": hyperparams.lora_dropout,
            "max_seq_len": hyperparams.max_seq_len,
        },
        "output_dir": output_dir,
        "merge_adapter": merge_adapter,
    });

    let base = endpoint.trim_end_matches('/').to_string();
    let job_id = {
        let url = format!("{}/train", base);
        tokio::task::spawn_blocking(move || post_train(&url, train_body)).await??
    };

    // 4. Pętla pollingu. spawn_blocking na każde GET, sleep w async między nimi.
    let deadline = tokio::time::Instant::now() + JOB_TIMEOUT;
    let status_url = format!("{}/status/{}", base, job_id);
    loop {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("LLM training timed out after {}s", JOB_TIMEOUT.as_secs());
        }
        tokio::time::sleep(POLL_INTERVAL).await;

        let url = status_url.clone();
        let st = tokio::task::spawn_blocking(move || get_status(&url)).await??;

        // Zapis metryk na żywo: dla bieżącego kroku rejestrujemy train/eval loss,
        // żeby UI mogło rysować krzywą jeszcze w trakcie treningu.
        if let Some(loss) = st.train_loss {
            repository::record_training_metric(run_id, st.step, "train_loss", loss)?;
        }
        if let Some(loss) = st.eval_loss {
            repository::record_training_metric(run_id, st.step, "eval_loss", loss)?;
        }

        match st.status.as_str() {
            "running" => continue,
            "succeeded" => {
                let metrics_json = json!({
                    "train_loss": st.train_loss,
                    "eval_loss": st.eval_loss,
                    "step": st.step,
                    "total_steps": st.total_steps,
                    "artifact_path": st.artifact_path,
                })
                .to_string();
                let model_name = format!("{}-{}", base_model, method);
                let model_id = repository::insert_model(
                    project_id,
                    &model_name,
                    "huggingface",
                    base_model,
                    &metrics_json,
                )?;
                repository::set_training_run_model(run_id, &model_id)?;
                repository::update_training_run_status(run_id, "succeeded")?;
                return Ok(());
            }
            "failed" => {
                let msg = st
                    .error
                    .unwrap_or_else(|| "ml-training reported failure without detail".to_string());
                anyhow::bail!("LLM training failed: {}", msg);
            }
            other => anyhow::bail!("ml-training returned unknown status '{}'", other),
        }
    }
}

/// Rozwiązuje endpoint serwisu ml-training z rejestru CORE (kategoria
/// `training`, engine `ml-training`). Pool przez `global_pool`, bo task nie ma
/// dostępu do `ctx.state.db`.
fn resolve_endpoint() -> anyhow::Result<String> {
    let pool = crate::db::global_pool()
        .ok_or_else(|| anyhow::anyhow!("core service registry unavailable"))?;
    let conn = pool
        .lock()
        .map_err(|_| anyhow::anyhow!("core db pool poisoned"))?;
    let svcs = services_repo::services::list_by_category(&conn, "training", Some("ml-training"))?;
    let svc = svcs.into_iter().next().ok_or_else(|| {
        anyhow::anyhow!("Serwis ml-training niedostępny — uruchom go w Serwisach")
    })?;
    svc.endpoint_url
        .ok_or_else(|| anyhow::anyhow!("serwis ml-training bez endpointu"))
}

/// Parsuje surowy dataset na listę rekordów dla serwisu. JSONL (linie JSON)
/// przekazujemy 1:1 jako dicty. CSV/XLSX mapujemy na `{prompt,response}` gdy są
/// takie kolumny, inaczej pierwsza kolumna jako `{text}` (minimalny, rozsądny
/// kontrakt zgodny z `_format_record` serwisu).
fn parse_records(raw: &[u8], kind: &str) -> anyhow::Result<Vec<Record>> {
    let kind = kind.to_ascii_lowercase();
    if kind == "jsonl" || kind == "json" {
        return parse_jsonl(raw);
    }
    // Jeśli kind nieznany, spróbuj wykryć JSONL po treści (pierwsza niepusta
    // linia parsuje się jako obiekt JSON) — inaczej traktuj jak CSV.
    if looks_like_jsonl(raw) {
        return parse_jsonl(raw);
    }
    let ext = if kind == "xlsx" { "xlsx" } else { "csv" };
    let filename = format!("dataset.{}", ext);
    let (headers, rows) = crate::ml_studio::profile::parse_table(raw, &filename)?;
    Ok(map_tabular(&headers, &rows))
}

/// Heurystyka: czy bajty wyglądają jak JSONL (pierwsza niepusta linia to obiekt).
fn looks_like_jsonl(raw: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(raw) else {
        return false;
    };
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).is_ok() && l.starts_with('{'))
        .unwrap_or(false)
}

/// Parsuje JSONL: każda niepusta linia to jeden rekord-obiekt JSON.
fn parse_jsonl(raw: &[u8]) -> anyhow::Result<Vec<Record>> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| anyhow::anyhow!("dataset is not valid UTF-8 JSONL"))?;
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|e| anyhow::anyhow!("JSONL line {} is not valid JSON: {}", idx + 1, e))?;
        if !value.is_object() {
            anyhow::bail!("JSONL line {} is not a JSON object", idx + 1);
        }
        out.push(value);
    }
    Ok(out)
}

/// Mapuje tabelę (nagłówki + wiersze) na rekordy. Kolejność detekcji kolumn:
/// `prompt`+`chosen`+`rejected` → para preferencji DPO; `prompt`+`response` →
/// rekord SFT; inaczej pierwsza kolumna jako `{text}`. JSONL omija tę funkcję
/// (przekazywany 1:1), więc to jedyna ścieżka mapowania CSV/XLSX.
fn map_tabular(headers: &[String], rows: &[Vec<String>]) -> Vec<Record> {
    let find = |name: &str| headers.iter().position(|h| h.eq_ignore_ascii_case(name));
    let prompt_idx = find("prompt");
    let response_idx = find("response");
    let chosen_idx = find("chosen");
    let rejected_idx = find("rejected");

    let cell = |row: &[String], idx: usize| row.get(idx).cloned().unwrap_or_default();

    rows.iter()
        .filter_map(|row| match (prompt_idx, chosen_idx, rejected_idx, response_idx) {
            (Some(pi), Some(ci), Some(ri), _) => Some(json!({
                "prompt": cell(row, pi),
                "chosen": cell(row, ci),
                "rejected": cell(row, ri),
            })),
            (Some(pi), _, _, Some(ri)) => Some(json!({
                "prompt": cell(row, pi),
                "response": cell(row, ri),
            })),
            _ => row.first().map(|text| json!({ "text": text })),
        })
        .collect()
}

/// Dzieli rekordy na train/eval ~90/10. Eval jest `None` gdy za mało rekordów
/// (≤ 1), bo split nie ma wtedy sensu.
fn split_eval(mut records: Vec<Record>) -> (Vec<Record>, Option<Vec<Record>>) {
    if records.len() < 10 {
        return (records, None);
    }
    let eval_count = (records.len() / 10).max(1);
    let split_at = records.len() - eval_count;
    let eval = records.split_off(split_at);
    (records, Some(eval))
}

/// Odpowiedź `POST /train`.
#[derive(serde::Deserialize)]
struct TrainResponse {
    job_id: String,
}

/// Odpowiedź `GET /status/{job_id}`.
#[derive(serde::Deserialize)]
struct StatusResponse {
    status: String,
    #[serde(default)]
    step: i64,
    #[serde(default)]
    total_steps: i64,
    #[serde(default)]
    train_loss: Option<f64>,
    #[serde(default)]
    eval_loss: Option<f64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    artifact_path: Option<String>,
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into()
}

/// Synchronous (blocking ureq) POST /train. Wywoływane w spawn_blocking.
fn post_train(url: &str, body: serde_json::Value) -> anyhow::Result<String> {
    let http = http_agent();
    let mut resp = http
        .post(url)
        .send_json(&body)
        .map_err(|e| anyhow::anyhow!("POST {} failed: {}", url, e))?;
    let parsed: TrainResponse = resp
        .body_mut()
        .read_json()
        .map_err(|e| anyhow::anyhow!("decode /train response: {}", e))?;
    Ok(parsed.job_id)
}

/// Synchronous (blocking ureq) GET /status. Wywoływane w spawn_blocking.
fn get_status(url: &str) -> anyhow::Result<StatusResponse> {
    let http = http_agent();
    let mut resp = http
        .get(url)
        .call()
        .map_err(|e| anyhow::anyhow!("GET {} failed: {}", url, e))?;
    resp.body_mut()
        .read_json()
        .map_err(|e| anyhow::anyhow!("decode /status response: {}", e))
}
