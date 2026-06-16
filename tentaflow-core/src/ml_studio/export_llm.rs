// ===== File: ml_studio/export_llm.rs — async LLM export-to-GGUF background task =====
//
// Asynchroniczny eksport wytrenowanego modelu FT do GGUF. Merge adaptera i
// konwersja do GGUF trwają (q8_0 ~ pół minuty+, f16 więcej), więc NIE mogą
// blokować RPC: handler ustawia na modelu `export_status="running"`, woła
// `spawn_ft_export` i wraca natychmiast; cała robota (start jobu w serwisie
// ml-training, polling, zapis ścieżki GGUF) dzieje się w `tokio::spawn`.
//
// Stan eksportu trzymamy w `models.metrics_json` (NIE osobna tabela): parsujemy
// istniejący obiekt JSON i wmergowujemy pola `export_status`, `gguf_path`,
// `gguf_size_bytes`, `export_error`. UI odpytuje je przez
// `MlStudioFtExportStatusRequest`.
//
// Źródła danych w tasku: task NIE ma `HandlerContext`, więc service discovery
// idzie przez `crate::db::global_pool()`, a dane ML Studio przez `repository::*`.
// HTTP: `ureq` jest BLOKUJĄCY — każde żądanie opakowane w `spawn_blocking`,
// między pollami `tokio::time::sleep`.

use std::time::Duration;

use serde_json::{json, Value};

use crate::ml_studio::repository;
use crate::services_repo;

/// Odstęp między kolejnymi odpytaniami `GET /export_status/{id}` serwisu.
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// Twardy limit całego eksportu (merge + konwersja GGUF). Po przekroczeniu task
/// przerywa polling i oznacza eksport jako `failed`, żeby „wiszący" job nie
/// został na zawsze w `running` na modelu.
const JOB_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Timeout pojedynczego żądania HTTP (start eksportu / pojedynczy status).
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);

/// Startuje eksport GGUF w tle. Model musi już mieć ustawione
/// `export_status="running"` w `metrics_json` (robi to handler). Funkcja wraca
/// natychmiast — błędy lądują w stanie eksportu na modelu (`failed` +
/// `export_error`), nie są propagowane.
pub fn spawn_ft_export(
    model_id: String,
    base_model: String,
    adapter_path: String,
    outtype: String,
    current_metrics_json: String,
) {
    tokio::spawn(async move {
        if let Err(err) = run_export(
            &model_id,
            &base_model,
            &adapter_path,
            &outtype,
            &current_metrics_json,
        )
        .await
        {
            tracing::warn!(model_id = %model_id, error = %err, "GGUF export failed");
            // Zapisz stan błędu na modelu, żeby UI zobaczyło `failed` + komunikat.
            let merged = merge_export_state(
                &current_metrics_json,
                "failed",
                None,
                None,
                Some(&err.to_string()),
            );
            let _ = repository::update_model_metrics(&model_id, &merged);
        }
    });
}

async fn run_export(
    model_id: &str,
    base_model: &str,
    adapter_path: &str,
    outtype: &str,
    current_metrics_json: &str,
) -> anyhow::Result<()> {
    // 1. Service discovery: serwis ml-training z kategorii "training". Pool CORE
    //    przez `global_pool` (task nie ma `ctx.state.db`).
    let endpoint = resolve_endpoint()?;
    let base = endpoint.trim_end_matches('/').to_string();

    // 2. POST /export w spawn_blocking (ureq jest blocking). Zwraca export_id.
    let export_body = json!({
        "adapter_path": adapter_path,
        "base_model": base_model,
        "outtype": outtype,
    });
    let export_id = {
        let url = format!("{}/export", base);
        tokio::task::spawn_blocking(move || post_export(&url, export_body)).await??
    };

    // 3. Pętla pollingu. spawn_blocking na każde GET, sleep w async między nimi.
    let deadline = tokio::time::Instant::now() + JOB_TIMEOUT;
    let status_url = format!("{}/export_status/{}", base, export_id);
    loop {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("GGUF export timed out after {}s", JOB_TIMEOUT.as_secs());
        }
        tokio::time::sleep(POLL_INTERVAL).await;

        let url = status_url.clone();
        let st = tokio::task::spawn_blocking(move || get_export_status(&url)).await??;

        match st.status.as_str() {
            "running" => continue,
            "succeeded" => {
                let gguf_path = st.gguf_path.ok_or_else(|| {
                    anyhow::anyhow!("ml-training reported success without gguf_path")
                })?;
                let merged = merge_export_state(
                    current_metrics_json,
                    "succeeded",
                    Some(&gguf_path),
                    st.size_bytes,
                    None,
                );
                repository::update_model_metrics(model_id, &merged)?;
                return Ok(());
            }
            "failed" => {
                let msg = st
                    .error
                    .unwrap_or_else(|| "ml-training reported failure without detail".to_string());
                anyhow::bail!("GGUF export failed: {}", msg);
            }
            other => anyhow::bail!("ml-training returned unknown export status '{}'", other),
        }
    }
}

/// Wmergowuje stan eksportu w istniejący obiekt `metrics_json`. Parsuje bieżący
/// JSON (gdy niepoprawny lub nie-obiekt, startuje z pustego obiektu, żeby nie
/// zgubić zapisu stanu), dokłada pola eksportu i serializuje z powrotem.
fn merge_export_state(
    current_metrics_json: &str,
    status: &str,
    gguf_path: Option<&str>,
    size_bytes: Option<u64>,
    error: Option<&str>,
) -> String {
    let mut root = match serde_json::from_str::<Value>(current_metrics_json) {
        Ok(Value::Object(map)) => Value::Object(map),
        _ => json!({}),
    };
    let obj = root
        .as_object_mut()
        .expect("root is an object by construction");
    obj.insert("export_status".to_string(), json!(status));
    obj.insert("gguf_path".to_string(), json!(gguf_path));
    obj.insert("gguf_size_bytes".to_string(), json!(size_bytes));
    obj.insert("export_error".to_string(), json!(error));
    root.to_string()
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

/// Odpowiedź `POST /export`.
#[derive(serde::Deserialize)]
struct ExportResponse {
    export_id: String,
}

/// Odpowiedź `GET /export_status/{id}`.
#[derive(serde::Deserialize)]
struct ExportStatusResponse {
    status: String,
    #[serde(default)]
    gguf_path: Option<String>,
    #[serde(default)]
    size_bytes: Option<u64>,
    #[serde(default)]
    error: Option<String>,
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into()
}

/// Synchronous (blocking ureq) POST /export. Wywoływane w spawn_blocking.
fn post_export(url: &str, body: Value) -> anyhow::Result<String> {
    let http = http_agent();
    let mut resp = http
        .post(url)
        .send_json(&body)
        .map_err(|e| anyhow::anyhow!("POST {} failed: {}", url, e))?;
    let parsed: ExportResponse = resp
        .body_mut()
        .read_json()
        .map_err(|e| anyhow::anyhow!("decode /export response: {}", e))?;
    Ok(parsed.export_id)
}

/// Synchronous (blocking ureq) GET /export_status. Wywoływane w spawn_blocking.
fn get_export_status(url: &str) -> anyhow::Result<ExportStatusResponse> {
    let http = http_agent();
    let mut resp = http
        .get(url)
        .call()
        .map_err(|e| anyhow::anyhow!("GET {} failed: {}", url, e))?;
    resp.body_mut()
        .read_json()
        .map_err(|e| anyhow::anyhow!("decode /export_status response: {}", e))
}
