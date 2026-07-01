// ===== File: ml_studio/train_classifier.rs — async atrybut-classifier training =====
//
// Asynchroniczny silnik treningu KLASYFIKATORA ATRYBUTU na wycinkach ML Studio
// (np. atrybut "stan" o wartościach czysta/brudna). Trening trwa MINUTY, więc —
// jak detekcja RF-DETR (`train_recognition.rs`) — NIE może blokować RPC. Handler
// tworzy run `running`, woła `spawn_classifier_training` i wraca natychmiast; cała
// robota (start jobu w serwisie `classifier-training`, polling, zapis metryk i
// modelu) dzieje się w tle.
//
// Wycinki (crops) z obrazów źródłowych buduje SERWIS Python — Core przekazuje mu
// tylko katalog datasetu COCO (`dataset_dir`) plus specyfikację atrybutu:
// `attribute`, `source_class` (kategoria COCO definiująca atrybut; "" = wszystkie
// klasy), `values` (kolejność = indeks etykiety) i `variant` backbone timm.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde_json::json;

use crate::ml_studio::repository;
use crate::services_repo;

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const JOB_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);

/// Startuje trening klasyfikatora w tle. Run o `run_id` musi już istnieć
/// (`running`). Błędy lądują w statusie runu (`failed`), nie są propagowane.
#[allow(clippy::too_many_arguments)]
pub fn spawn_classifier_training(
    run_id: String,
    project_id: String,
    owner_user_id: String,
    dataset_id: String,
    attribute: String,
    source_class: String,
    variant: String,
    values: Vec<String>,
    hyperparams: tentaflow_protocol::MlStudioClassifierHyperparams,
) {
    tokio::spawn(async move {
        if let Err(err) = run_training(
            &run_id,
            &project_id,
            &owner_user_id,
            &dataset_id,
            &attribute,
            &source_class,
            &variant,
            &values,
            &hyperparams,
        )
        .await
        {
            tracing::warn!(run_id = %run_id, error = %err, "classifier training failed");
            let _ = repository::set_training_run_error(&run_id, &err.to_string());
            let _ = repository::update_training_run_status(&run_id, "failed");
        }
        // Sprzątamy wpis live-view niezależnie od wyniku (job już nie żyje).
        crate::ml_studio::live_view::clear_local_job(&run_id);
    });
}

#[allow(clippy::too_many_arguments)]
async fn run_training(
    run_id: &str,
    project_id: &str,
    owner_user_id: &str,
    dataset_id: &str,
    attribute: &str,
    source_class: &str,
    variant: &str,
    values: &[String],
    hyperparams: &tentaflow_protocol::MlStudioClassifierHyperparams,
) -> anyhow::Result<()> {
    let endpoint = resolve_endpoint()?;

    let dataset = repository::get_dataset(owner_user_id, dataset_id)?
        .ok_or_else(|| anyhow::anyhow!("dataset not found"))?;
    let raw = repository::get_dataset_raw(owner_user_id, dataset_id)?;

    // Dwa źródła datasetu COCO (identycznie jak recognition):
    //  - "coco_path": raw to ŚCIEŻKA do katalogu COCO na dysku (duże zbiory),
    //  - "coco" (zip): rozpakowujemy bajty do katalogu cache.
    let dataset_dir = if dataset.kind == "coco_path" {
        let dir = std::path::PathBuf::from(String::from_utf8_lossy(&raw).trim().to_string());
        if !dir.is_dir() {
            anyhow::bail!("katalog datasetu COCO nie istnieje: {}", dir.display());
        }
        dir
    } else {
        let dir = crate::paths::cache_dir()
            .join("ml-classifier-datasets")
            .join(dataset_id);
        unpack_coco(&raw, &dir)?;
        dir
    };

    run_training_against_dir(
        run_id,
        project_id,
        attribute,
        source_class,
        variant,
        values,
        hyperparams,
        &endpoint,
        &dataset_dir,
    )
    .await
}

/// Startuje job na serwisie `classifier-training` dla katalogu datasetu COCO
/// (serwis sam buduje wycinki wg `attribute`/`source_class`) i odpytuje status do
/// końca. `dataset_dir` może pochodzić z lokalnego dysku albo z cache mesh.
#[allow(clippy::too_many_arguments)]
async fn run_training_against_dir(
    run_id: &str,
    project_id: &str,
    attribute: &str,
    source_class: &str,
    variant: &str,
    values: &[String],
    hyperparams: &tentaflow_protocol::MlStudioClassifierHyperparams,
    endpoint: &str,
    dataset_dir: &Path,
) -> anyhow::Result<()> {
    let output_dir = format!("classifier/{}/{}", project_id, run_id);
    let train_body = json!({
        "dataset_dir": dataset_dir.to_string_lossy(),
        "attribute": attribute,
        "source_class": source_class,
        "values": values,
        "output_dir": output_dir,
        "variant": variant,
        "hyperparams": {
            "epochs": hyperparams.epochs,
            "batch_size": hyperparams.batch_size,
            "learning_rate": hyperparams.learning_rate,
            "image_size": hyperparams.image_size,
            "freeze_backbone": hyperparams.freeze_backbone,
        },
    });

    let base = endpoint.trim_end_matches('/').to_string();
    let job_id = {
        let url = format!("{}/train", base);
        tokio::task::spawn_blocking(move || post_train(&url, train_body)).await??
    };
    // Rejestracja do live-view: handlery mogą teraz odpytać serwis o postęp.
    crate::ml_studio::live_view::register_local_job(run_id, &base, &job_id);

    let deadline = tokio::time::Instant::now() + JOB_TIMEOUT;
    let status_url = format!("{}/status/{}", base, job_id);
    loop {
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("classifier training timed out after {}s", JOB_TIMEOUT.as_secs());
        }
        tokio::time::sleep(POLL_INTERVAL).await;

        let url = status_url.clone();
        let st = tokio::task::spawn_blocking(move || get_status(&url)).await??;

        // Metryki per epoka: train_loss + val_acc + val_macro_f1 → krzywa w UI.
        if let Some(v) = st.train_loss {
            repository::record_training_metric(run_id, st.epoch, "train_loss", v)?;
        }
        if let Some(v) = st.val_acc {
            repository::record_training_metric(run_id, st.epoch, "val_acc", v)?;
        }
        if let Some(v) = st.val_macro_f1 {
            repository::record_training_metric(run_id, st.epoch, "val_macro_f1", v)?;
        }

        match st.status.as_str() {
            "running" => continue,
            "succeeded" => {
                let metrics_json = json!({
                    "task": "classifier",
                    "attribute": attribute,
                    "source_class": source_class,
                    "values": values,
                    "val_acc": st.val_acc,
                    "val_macro_f1": st.val_macro_f1,
                    "onnx_path": st.onnx_path,
                    "checkpoint_path": st.checkpoint_path,
                })
                .to_string();
                let model_name = format!("classifier-{}-{}", attribute, variant);
                let model_id = repository::insert_model(
                    project_id,
                    &model_name,
                    "classifier-timm",
                    variant,
                    &metrics_json,
                )?;
                repository::set_training_run_model(run_id, &model_id)?;
                repository::update_training_run_status(run_id, "succeeded")?;
                return Ok(());
            }
            "failed" => {
                let msg = st
                    .error
                    .unwrap_or_else(|| "classifier-training reported failure".to_string());
                anyhow::bail!("classifier training failed: {}", msg);
            }
            other => anyhow::bail!("classifier-training unknown status '{}'", other),
        }
    }
}

/// Rozpakowuje zip COCO do `dest` (czyści wcześniejszą zawartość). Serwis
/// klasyfikatora sam buduje wycinki z rozpakowanego katalogu COCO.
fn unpack_coco(zip_bytes: &[u8], dest: &Path) -> anyhow::Result<()> {
    if dest.exists() {
        let _ = std::fs::remove_dir_all(dest);
    }
    std::fs::create_dir_all(dest)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| anyhow::anyhow!("dataset nie jest poprawnym zip COCO: {}", e))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            continue; // odcięcie path traversal (zip slip)
        };
        let out_path = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        std::fs::write(&out_path, &buf)?;
    }
    Ok(())
}

// Rejestr jobów klasyfikatora uruchomionych PRZEZ MESH na tym nodzie (odbiorca).
// Mapuje `run_id` (klucz inicjatora Node A) na lokalny job serwisu (base+job_id).
// Osobny od rejestru recognition — router statusu (`is_classifier_mesh_job`)
// rozstrzyga, do którego toru należy dany `run_id`.
static MESH_JOBS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, (String, String)>>,
> = std::sync::OnceLock::new();

fn mesh_jobs() -> &'static std::sync::Mutex<std::collections::HashMap<String, (String, String)>> {
    MESH_JOBS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Czy `run_id` to job klasyfikatora zlecony tu przez mesh (do routera statusu).
pub fn is_classifier_mesh_job(run_id: &str) -> bool {
    mesh_jobs()
        .lock()
        .map(|m| m.contains_key(run_id))
        .unwrap_or(false)
}

/// B-side (odbiorca `MlTrainStart` z `kind="classifier"`): startuje trening na
/// LOKALNYM serwisie klasyfikatora wg `spec_json` (dataset_dir/attribute/
/// source_class/values/variant/output_dir/hyperparams) i zapamiętuje job pod
/// `run_id`. Dataset przynoszony przez mesh (content-addr) rozpakowuje wspólny
/// mechanizm recognition (`mesh_dataset_cache`); weryfikacja content-hasha jak
/// w recognition (ten sam zasób na obu nodach).
pub async fn mesh_train_start_classifier(run_id: &str, spec_json: &str) -> anyhow::Result<()> {
    let spec: serde_json::Value = serde_json::from_str(spec_json)
        .map_err(|e| anyhow::anyhow!("spec_json invalid: {}", e))?;
    let dataset_dir_raw = spec
        .get("dataset_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("spec bez dataset_dir"))?;
    // "mesh:<hash>" → dataset przyniesiony przez mesh, w cache content-addr.
    let resolved;
    let dataset_dir: &str = if let Some(hash) = dataset_dir_raw.strip_prefix("mesh:") {
        let c = crate::ml_studio::train_recognition::mesh_dataset_cache(hash);
        if !c.is_dir() {
            anyhow::bail!("dataset mesh nie zmaterializowany na tym nodzie (hash {})", hash);
        }
        resolved = c.to_string_lossy().to_string();
        &resolved
    } else {
        dataset_dir_raw
    };
    if !std::path::Path::new(dataset_dir).is_dir() {
        anyhow::bail!("dataset niedostępny na tym nodzie ({})", dataset_dir);
    }
    if let Some(expected) = spec.get("dataset_hash").and_then(|v| v.as_str()) {
        let actual = crate::ml_studio::train_recognition::coco_content_hash(
            std::path::Path::new(dataset_dir),
        )?;
        if actual != expected {
            anyhow::bail!(
                "dataset na tym nodzie to NIE ten sam zasób (hash mismatch: oczekiwano {}, jest {})",
                &expected[..expected.len().min(12)],
                &actual[..actual.len().min(12)]
            );
        }
    }

    let attribute = spec.get("attribute").and_then(|v| v.as_str()).unwrap_or("");
    let source_class = spec.get("source_class").and_then(|v| v.as_str()).unwrap_or("");
    let values = spec.get("values").cloned().unwrap_or(json!([]));
    let variant = spec
        .get("variant")
        .and_then(|v| v.as_str())
        .unwrap_or("mobilenetv4");
    let output_dir = spec
        .get("output_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("spec bez output_dir"))?;
    let hyperparams = spec.get("hyperparams").cloned().unwrap_or(json!({}));

    let endpoint = resolve_endpoint()?;
    let base = endpoint.trim_end_matches('/').to_string();
    let train_body = json!({
        "dataset_dir": dataset_dir,
        "attribute": attribute,
        "source_class": source_class,
        "values": values,
        "output_dir": output_dir,
        "variant": variant,
        "hyperparams": hyperparams,
    });
    let url = format!("{}/train", base);
    let job_id = tokio::task::spawn_blocking(move || post_train(&url, train_body)).await??;
    mesh_jobs()
        .lock()
        .map_err(|_| anyhow::anyhow!("mesh_jobs lock poisoned"))?
        .insert(run_id.to_string(), (base, job_id));
    Ok(())
}

/// B-side (odbiorca `MlTrainStatus`): odpytuje lokalny serwis o status joba
/// klasyfikatora zmapowanego z `run_id` i zwraca surowy JSON statusu do inicjatora.
pub async fn mesh_train_status_classifier(run_id: &str) -> anyhow::Result<String> {
    let (base, job_id) = mesh_jobs()
        .lock()
        .map_err(|_| anyhow::anyhow!("mesh_jobs lock poisoned"))?
        .get(run_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("nieznany run_id na tym nodzie: {}", run_id))?;
    let url = format!("{}/status/{}", base, job_id);
    let value: serde_json::Value = tokio::task::spawn_blocking(move || {
        let http = http_agent();
        let mut resp = http
            .get(&url)
            .call()
            .map_err(|e| anyhow::anyhow!("GET {} failed: {}", url, e))?;
        resp.body_mut()
            .read_json::<serde_json::Value>()
            .map_err(|e| anyhow::anyhow!("decode status: {}", e))
    })
    .await??;
    Ok(value.to_string())
}

fn resolve_endpoint() -> anyhow::Result<String> {
    let pool = crate::db::global_pool()
        .ok_or_else(|| anyhow::anyhow!("core service registry unavailable"))?;
    let conn = pool.read().map_err(|_| anyhow::anyhow!("core db read"))?;
    let svcs =
        services_repo::services::list_by_category(&conn, "training", Some("classifier-training"))?;
    let svc = svcs.into_iter().next().ok_or_else(|| {
        anyhow::anyhow!("Serwis classifier-training niedostępny — wdróż go w Serwisach")
    })?;
    svc.endpoint_url
        .ok_or_else(|| anyhow::anyhow!("serwis classifier-training bez endpointu"))
}

#[derive(serde::Deserialize)]
struct TrainResponse {
    job_id: String,
}

#[derive(serde::Deserialize)]
struct StatusResponse {
    status: String,
    #[serde(default)]
    epoch: i64,
    #[serde(default)]
    train_loss: Option<f64>,
    #[serde(default)]
    val_acc: Option<f64>,
    #[serde(default)]
    val_macro_f1: Option<f64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    onnx_path: Option<String>,
    #[serde(default)]
    checkpoint_path: Option<String>,
}

fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into()
}

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
