// ===== File: ml_studio/train_ocr.rs — async OCR reader training =====
//
// Asynchroniczny silnik treningu CZYTNIKA OCR tablic (CRNN + CTC) ML Studio.
// Trening trwa MINUTY, więc — jak detekcja RF-DETR (`train_recognition.rs`) i
// klasyfikator (`train_classifier.rs`) — NIE może blokować RPC. Handler tworzy run
// `running`, woła `spawn_ocr_training` i wraca natychmiast; cała robota (start jobu
// w serwisie `ocr-training`, polling, zapis metryk i modelu) dzieje się w tle.
//
// Wycinki, podział na wiersze i etykiety buduje SERWIS Python — Core przekazuje mu
// katalog datasetu COCO (`dataset_dir`), atrybut OCR (`attribute`), klasę źródłową
// (`source_class`) oraz KATALOG ADR wdrożenia (pary kemler/UN z `adr-list.json`),
// z którego serwis generuje wiersze syntetyczne. Realnych odczytów jest z natury
// mało, więc bez syntetyku model tylko zapamiętuje kilkaset etykiet.
//
// Po sukcesie treningu Core od razu robi eksport ONNX: artefaktem tego toru NIE
// jest checkpoint torcha, a para `adr_ocr.onnx` + `adr_ocr_alphabet.txt`, której
// szuka runtime (`vision::adr_ocr`). Bez eksportu model byłby nie do użycia.

use std::path::Path;
use std::time::Duration;

use serde_json::json;

use crate::ml_studio::repository;
use crate::services_repo;

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const JOB_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
/// Eksport buduje model i sprawdza zgodność z ONNX Runtime — własny, dłuższy
/// timeout niż pollingowy `HTTP_TIMEOUT`.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Startuje trening czytnika OCR w tle. Run o `run_id` musi już istnieć
/// (`running`). Błędy lądują w statusie runu (`failed`), nie są propagowane.
pub fn spawn_ocr_training(
    run_id: String,
    project_id: String,
    owner_user_id: String,
    dataset_id: String,
    attribute: String,
    source_class: String,
    hyperparams: tentaflow_protocol::MlStudioOcrHyperparams,
) {
    tokio::spawn(async move {
        if let Err(err) = run_training(
            &run_id,
            &project_id,
            &owner_user_id,
            &dataset_id,
            &attribute,
            &source_class,
            &hyperparams,
        )
        .await
        {
            tracing::warn!(run_id = %run_id, error = %err, "OCR training failed");
            let _ = repository::set_training_run_error(&run_id, &err.to_string());
            let _ = repository::update_training_run_status(&run_id, "failed");
        }
        // Sprzątamy wpis live-view niezależnie od wyniku (job już nie żyje).
        crate::ml_studio::live_view::clear_local_job(&run_id);
    });
}

async fn run_training(
    run_id: &str,
    project_id: &str,
    owner_user_id: &str,
    dataset_id: &str,
    attribute: &str,
    source_class: &str,
    hyperparams: &tentaflow_protocol::MlStudioOcrHyperparams,
) -> anyhow::Result<()> {
    let endpoint = resolve_endpoint()?;

    let dataset = repository::get_dataset(owner_user_id, dataset_id)?
        .ok_or_else(|| anyhow::anyhow!("dataset not found"))?;
    let raw = repository::get_dataset_raw(owner_user_id, dataset_id)?;

    // Dwa źródła datasetu COCO (identycznie jak recognition/classifier):
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
            .join("ml-ocr-datasets")
            .join(dataset_id);
        unpack_coco(&raw, &dir)?;
        dir
    };

    run_training_against_dir(
        run_id,
        project_id,
        attribute,
        source_class,
        hyperparams,
        &endpoint,
        &dataset_dir,
    )
    .await
}

/// Pary kemler/UN katalogu ADR wdrożenia w formie przyjmowanej przez serwis.
/// Pusta lista = brak `adr-list.json`; wołający sprawdza to PRZED startem jobu,
/// bo syntetyk bez katalogu uczy się wyłącznie losowych cyfr.
pub fn adr_pairs_json() -> Vec<serde_json::Value> {
    crate::vision::adr::pary_kemler_un()
        .into_iter()
        .map(|(kemler, un)| json!({"kemler": kemler, "un": un}))
        .collect()
}

/// Startuje job na serwisie `ocr-training` dla katalogu datasetu COCO, odpytuje
/// status do końca, a po sukcesie eksportuje model do ONNX i zapisuje wpis modelu.
async fn run_training_against_dir(
    run_id: &str,
    project_id: &str,
    attribute: &str,
    source_class: &str,
    hyperparams: &tentaflow_protocol::MlStudioOcrHyperparams,
    endpoint: &str,
    dataset_dir: &Path,
) -> anyhow::Result<()> {
    let output_dir = format!("ocr/{}/{}", project_id, run_id);
    let train_body = json!({
        "dataset_dir": dataset_dir.to_string_lossy(),
        "attribute": attribute,
        "source_class": source_class,
        "output_dir": output_dir,
        "adr_pairs": adr_pairs_json(),
        "hyperparams": ocr_hyperparams_json(hyperparams),
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
            anyhow::bail!("OCR training timed out after {}s", JOB_TIMEOUT.as_secs());
        }
        // Anulowanie zgłoszone przez użytkownika: serwis dostał już `POST /cancel`
        // od handlera, więc pętla nadzoru nie ma czego pilnować.
        if crate::ml_studio::live_view::is_cancel_requested(run_id) {
            repository::update_training_run_status(run_id, "cancelled")?;
            crate::ml_studio::live_view::clear_cancel(run_id);
            return Ok(());
        }
        tokio::time::sleep(POLL_INTERVAL).await;

        let url = status_url.clone();
        let st = tokio::task::spawn_blocking(move || get_status(&url)).await??;

        // Metryki per epoka: train_loss + exact-match na realnych i syntetycznych
        // wierszach → krzywa w UI.
        if let Some(v) = st.train_loss {
            repository::record_training_metric(run_id, st.epoch, "train_loss", v)?;
        }
        if let Some(v) = st.val_exact_real {
            repository::record_training_metric(run_id, st.epoch, "val_exact_real", v)?;
        }
        if let Some(v) = st.val_exact_synth {
            repository::record_training_metric(run_id, st.epoch, "val_exact_synth", v)?;
        }

        match st.status.as_str() {
            "running" => continue,
            "cancelled" => {
                repository::update_training_run_status(run_id, "cancelled")?;
                crate::ml_studio::live_view::clear_cancel(run_id);
                return Ok(());
            }
            "succeeded" => {
                let checkpoint = st.artifact_path.clone().ok_or_else(|| {
                    anyhow::anyhow!("serwis zgłosił sukces bez ścieżki checkpointu")
                })?;
                // Eksport od razu: artefaktem tego toru jest ONNX + alfabet, nie
                // checkpoint torcha. Porażka eksportu = porażka runu (model
                // niedostępny dla runtime'u to model, którego nie ma).
                let exported = run_export(&base, &checkpoint, &output_dir).await?;
                let metrics_json = json!({
                    "task": "ocr",
                    "attribute": attribute,
                    "source_class": source_class,
                    "val_exact_real": st.val_exact_real,
                    "val_exact_synth": st.val_exact_synth,
                    "checkpoint_path": checkpoint,
                    "onnx_path": exported.onnx_path,
                    "alphabet_path": exported.alphabet_path,
                })
                .to_string();
                let model_name = format!("ocr-crnn-{}", attribute);
                let model_id = repository::insert_model(
                    project_id,
                    &model_name,
                    "ocr-crnn",
                    "CRNN + CTC",
                    &metrics_json,
                )?;
                repository::set_training_run_model(run_id, &model_id)?;
                repository::update_training_run_status(run_id, "succeeded")?;
                return Ok(());
            }
            "failed" => {
                let msg = st
                    .error
                    .unwrap_or_else(|| "ocr-training reported failure".to_string());
                anyhow::bail!("OCR training failed: {}", msg);
            }
            other => anyhow::bail!("ocr-training unknown status '{}'", other),
        }
    }
}

fn ocr_hyperparams_json(hp: &tentaflow_protocol::MlStudioOcrHyperparams) -> serde_json::Value {
    json!({
        "epochs": hp.epochs,
        "batch_size": hp.batch_size,
        "learning_rate": hp.learning_rate,
        "synthetic_per_epoch": hp.synthetic_per_epoch,
        "real_repeat": hp.real_repeat,
    })
}

/// Wynik eksportu: ścieżki plików, których szuka runtime czytnika.
pub struct OcrExport {
    pub onnx_path: String,
    pub alphabet_path: String,
}

/// Eksportuje checkpoint do ONNX przez `POST /export` serwisu. Serwis sam
/// weryfikuje zgodność liczbową torch↔onnxruntime i odrzuca rozjechany eksport.
async fn run_export(base: &str, checkpoint_path: &str, output_dir: &str) -> anyhow::Result<OcrExport> {
    let url = format!("{}/export", base.trim_end_matches('/'));
    let body = json!({
        "checkpoint_path": checkpoint_path,
        "output_dir": output_dir,
    });
    let value: serde_json::Value =
        tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
            let http: ureq::Agent = ureq::Agent::config_builder()
                .timeout_global(Some(EXPORT_TIMEOUT))
                .build()
                .into();
            let mut resp = http
                .post(&url)
                .send_json(&body)
                .map_err(|e| anyhow::anyhow!("POST {} failed: {}", url, e))?;
            resp.body_mut()
                .read_json()
                .map_err(|e| anyhow::anyhow!("decode /export response: {}", e))
        })
        .await??;
    let onnx_path = value
        .get("onnx_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("/export response without onnx_path"))?
        .to_string();
    let alphabet_path = value
        .get("alphabet_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("/export response without alphabet_path"))?
        .to_string();
    Ok(OcrExport {
        onnx_path,
        alphabet_path,
    })
}

/// Rozpakowuje zip COCO do `dest` (czyści wcześniejszą zawartość). Serwis OCR sam
/// buduje wycinki i wiersze z rozpakowanego katalogu.
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
        std::io::Read::read_to_end(&mut entry, &mut buf)?;
        std::fs::write(&out_path, &buf)?;
    }
    Ok(())
}

// Rejestr jobów OCR uruchomionych PRZEZ MESH na tym nodzie (odbiorca). Mapuje
// `run_id` (klucz inicjatora Node A) na lokalny job serwisu (base+job_id).
// Osobny od rejestrów recognition/classifier — router statusu (`is_ocr_mesh_job`)
// rozstrzyga, do którego toru należy dany `run_id`.
static MESH_JOBS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, (String, String)>>,
> = std::sync::OnceLock::new();

fn mesh_jobs() -> &'static std::sync::Mutex<std::collections::HashMap<String, (String, String)>> {
    MESH_JOBS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Czy `run_id` to job OCR zlecony tu przez mesh (do routera statusu).
pub fn is_ocr_mesh_job(run_id: &str) -> bool {
    mesh_jobs()
        .lock()
        .map(|m| m.contains_key(run_id))
        .unwrap_or(false)
}

/// B-side (odbiorca `MlTrainStart` z `kind="ocr"`): startuje trening na LOKALNYM
/// serwisie OCR wg `spec_json` i zapamiętuje job pod `run_id`. Dataset przyniesiony
/// przez mesh (content-addr) rozpakowuje wspólny mechanizm recognition
/// (`mesh_dataset_cache`); weryfikacja content-hasha jak w recognition.
///
/// `adr_pairs` bierzemy z katalogu WĘZŁA TRENUJĄCEGO, nie z requestu: to jego
/// `adr-list.json` opisuje wdrożenie, w którym model będzie działał.
pub async fn mesh_train_start_ocr(run_id: &str, spec_json: &str) -> anyhow::Result<()> {
    let spec: serde_json::Value =
        serde_json::from_str(spec_json).map_err(|e| anyhow::anyhow!("spec_json invalid: {}", e))?;
    let dataset_dir_raw = spec
        .get("dataset_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("spec bez dataset_dir"))?;
    let resolved;
    let dataset_dir: &str = if let Some(hash) = dataset_dir_raw.strip_prefix("mesh:") {
        let c = crate::ml_studio::train_recognition::mesh_dataset_cache(hash);
        if !c.is_dir() {
            anyhow::bail!(
                "dataset mesh nie zmaterializowany na tym nodzie (hash {})",
                hash
            );
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
        let actual = crate::ml_studio::train_recognition::coco_content_hash(std::path::Path::new(
            dataset_dir,
        ))?;
        if actual != expected {
            anyhow::bail!(
                "dataset na tym nodzie to NIE ten sam zasób (hash mismatch: oczekiwano {}, jest {})",
                &expected[..expected.len().min(12)],
                &actual[..actual.len().min(12)]
            );
        }
    }

    let attribute = spec.get("attribute").and_then(|v| v.as_str()).unwrap_or("");
    let source_class = spec
        .get("source_class")
        .and_then(|v| v.as_str())
        .unwrap_or("");
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
        "output_dir": output_dir,
        "adr_pairs": adr_pairs_json(),
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

/// B-side (odbiorca `MlTrainStatus`): surowy JSON statusu joba OCR zmapowanego
/// z `run_id`, zwracany do inicjatora.
pub async fn mesh_train_status_ocr(run_id: &str) -> anyhow::Result<String> {
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

/// B-side (odbiorca `MlTrainCancel`): woła `/cancel` lokalnego serwisu OCR.
pub async fn mesh_train_cancel_ocr(run_id: &str) -> anyhow::Result<()> {
    let (base, job_id) = mesh_jobs()
        .lock()
        .map_err(|_| anyhow::anyhow!("mesh_jobs lock poisoned"))?
        .get(run_id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("nieznany run_id na tym nodzie: {}", run_id))?;
    let ok = tokio::task::spawn_blocking(move || {
        crate::ml_studio::live_view::cancel_service_job_blocking(&base, &job_id)
    })
    .await?;
    if !ok {
        anyhow::bail!("serwis ocr-training odrzucił żądanie anulowania");
    }
    Ok(())
}

fn resolve_endpoint() -> anyhow::Result<String> {
    let pool = crate::db::global_pool()
        .ok_or_else(|| anyhow::anyhow!("core service registry unavailable"))?;
    let conn = pool.read().map_err(|_| anyhow::anyhow!("core db read"))?;
    let svcs = services_repo::services::list_by_category(&conn, "training", Some("ocr-training"))?;
    let svc = svcs
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("Serwis ocr-training niedostępny — wdróż go w Serwisach"))?;
    svc.endpoint_url
        .ok_or_else(|| anyhow::anyhow!("serwis ocr-training bez endpointu"))
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
    val_exact_real: Option<f64>,
    #[serde(default)]
    val_exact_synth: Option<f64>,
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
