// ===== File: ml_studio/train_recognition.rs — async RF-DETR detection training =====
//
// Asynchroniczny silnik treningu detekcji obiektów (RF-DETR) ML Studio. Trening
// detekcji trwa MINUTY/GODZINY, więc — jak fine-tuning LLM (`train_llm.rs`) — NIE
// może blokować RPC. Handler tworzy run `running`, woła `spawn_recog_training` i
// wraca natychmiast; cała robota (rozpakowanie datasetu COCO, start jobu w
// serwisie rfdetr-training, polling, zapis metryk i modelu) dzieje się w tle.
//
// Dataset recognition to ZIP COCO (obrazy + `_annotations.coco.json` w
// podkatalogach train/valid/test). Rozpakowujemy go do katalogu cache na tym
// samym węźle co serwis treningowy i przekazujemy `dataset_dir` w POST /train.
// Nazwy klas wyciągamy z `categories` w COCO json (kolejność po id rosnąco).

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use base64::Engine;
use serde_json::json;

use crate::ml_studio::repository;
use crate::services_repo;

const POLL_INTERVAL: Duration = Duration::from_secs(5);
const JOB_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);

/// Snapuje rozdzielczość treningu do najbliższej wielokrotności wymaganej przez
/// backbone danego wariantu RF-DETR — bo serwis rfdetr-training waliduje ten
/// warunek i odrzuca job z błędem `resolution ... not divisible by ...`.
///
/// - `base`/`large` używają backbone DINOv2 (patch_size 14 * num_windows 4 = 56),
///   więc rozdzielczość musi być wielokrotnością **56** (np. 560, 616, 672).
/// - `nano`/`small`/`medium` używają windowed attention (patch_size 16 * num_windows
///   2 = 32), więc wystarczy wielokrotność **32** (np. 576).
///
/// Snap jest do NAJBLIŻSZEJ wielokrotności: dla base wejście 560 → 560 (już pasuje),
/// a 576 → 560 (bo 576 leży bliżej 560 niż 616: |576-560|=16 < |576-616|=40).
fn snap_resolution(resolution: u32, variant: &str) -> u32 {
    let step: i64 = match variant {
        "base" | "large" => 56,
        _ => 32,
    };
    // Zaokrąglenie do najbliższej wielokrotności `step`, z dolnym limitem 224
    // (najmniejsza sensowna rozdzielczość; jest wielokrotnością i 32, i 56).
    let r = (resolution as i64).max(224);
    (((r + step / 2) / step) * step) as u32
}

/// Startuje trening detekcji w tle. Run o `run_id` musi już istnieć (`running`).
/// Błędy lądują w statusie runu (`failed`), nie są propagowane.
#[allow(clippy::too_many_arguments)]
pub fn spawn_recog_training(
    run_id: String,
    project_id: String,
    owner_user_id: String,
    dataset_id: String,
    variant: String,
    hyperparams: tentaflow_protocol::MlStudioRecogHyperparams,
) {
    tokio::spawn(async move {
        if let Err(err) = run_training(
            &run_id,
            &project_id,
            &owner_user_id,
            &dataset_id,
            &variant,
            &hyperparams,
        )
        .await
        {
            tracing::warn!(run_id = %run_id, error = %err, "RF-DETR training failed");
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
    variant: &str,
    hyperparams: &tentaflow_protocol::MlStudioRecogHyperparams,
) -> anyhow::Result<()> {
    let endpoint = resolve_endpoint()?;

    let dataset = repository::get_dataset(owner_user_id, dataset_id)?
        .ok_or_else(|| anyhow::anyhow!("dataset not found"))?;
    let raw = repository::get_dataset_raw(owner_user_id, dataset_id)?;

    // Dwa źródła datasetu COCO:
    //  - "coco_path": raw to ŚCIEŻKA do katalogu COCO na dysku (duże zbiory),
    //  - "coco" (zip): rozpakowujemy bajty do katalogu cache.
    let (dataset_dir, class_names) = if dataset.kind == "coco_path" {
        let dir = std::path::PathBuf::from(String::from_utf8_lossy(&raw).trim().to_string());
        if !dir.is_dir() {
            anyhow::bail!("katalog datasetu COCO nie istnieje: {}", dir.display());
        }
        let classes = coco_class_names_from_dir(&dir);
        (dir, classes)
    } else {
        let dir = crate::paths::cache_dir()
            .join("ml-recog-datasets")
            .join(dataset_id);
        let classes = unpack_coco(&raw, &dir)?;
        (dir, classes)
    };
    if class_names.is_empty() {
        anyhow::bail!("dataset COCO bez kategorii (class_names puste)");
    }

    // RF-DETR `/train` wymaga splitów train/ ORAZ valid/. Build dataset + auto-label
    // dają tylko train/, więc gdy brak valid/ tworzymy EFEMERYCZNĄ kopię ze
    // wstrzymanym splitem walidacyjnym. Oryginalny dataset (który użytkownik dalej
    // poprawia w edytorze) NIGDY nie jest modyfikowany.
    let prepared = prepare_dataset_with_valid(&dataset_dir, run_id)?;
    // Sprzątanie efemerycznego splitu po zakończeniu joba (sukces/porażka) — to
    // dane pochodne, nie dataset użytkownika. Wykonujemy ręcznie na każdej ścieżce
    // wyjścia poniżej (zamiast guarda Drop, by uniknąć blokowania w destruktorze).
    let result = run_training_against_dir(
        run_id,
        project_id,
        variant,
        hyperparams,
        &endpoint,
        prepared.train_dir(),
        &class_names,
    )
    .await;
    prepared.cleanup();
    result
}

/// Wynik przygotowania katalogu treningowego: albo oryginalny `coco_path` (gdy
/// miał już valid/), albo efemeryczna kopia ze splitem train/valid do sprzątnięcia.
enum PreparedDataset {
    /// Oryginalny katalog datasetu — przekazujemy bez zmian.
    Original(std::path::PathBuf),
    /// Efemeryczny katalog ze splitem train/valid (do usunięcia po treningu).
    Ephemeral(std::path::PathBuf),
}

impl PreparedDataset {
    fn train_dir(&self) -> &Path {
        match self {
            PreparedDataset::Original(p) | PreparedDataset::Ephemeral(p) => p,
        }
    }

    fn cleanup(&self) {
        if let PreparedDataset::Ephemeral(p) = self {
            let _ = std::fs::remove_dir_all(p);
        }
    }
}

/// Startuje job na serwisie rfdetr-training dla GOTOWEGO `dataset_dir` (zawiera już
/// train/ + valid/) i odpytuje status do końca. Wydzielone z `run_training`, żeby
/// efemeryczny split można było sprzątnąć na każdej ścieżce wyjścia (`?` w środku).
async fn run_training_against_dir(
    run_id: &str,
    project_id: &str,
    variant: &str,
    hyperparams: &tentaflow_protocol::MlStudioRecogHyperparams,
    endpoint: &str,
    dataset_dir: &Path,
    class_names: &[String],
) -> anyhow::Result<()> {
    let output_dir = format!("recog/{}/{}", project_id, run_id);
    // Snap rozdzielczości do wielokrotności wymaganej przez backbone danego wariantu
    // (patrz `snap_resolution`), aby czysty trening nigdy nie padał na walidacji.
    let resolution = snap_resolution(hyperparams.resolution, variant);
    let train_body = json!({
        "dataset_dir": dataset_dir.to_string_lossy(),
        "class_names": class_names,
        "variant": variant,
        "output_dir": output_dir,
        "hyperparams": {
            "epochs": hyperparams.epochs,
            "batch_size": hyperparams.batch_size,
            "grad_accum": hyperparams.grad_accum,
            "lr": hyperparams.learning_rate,
            "resolution": resolution,
            "early_stopping": hyperparams.early_stopping,
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
            anyhow::bail!("RF-DETR training timed out after {}s", JOB_TIMEOUT.as_secs());
        }
        tokio::time::sleep(POLL_INTERVAL).await;

        let url = status_url.clone();
        let st = tokio::task::spawn_blocking(move || get_status(&url)).await??;

        // Metryki per epoka: train_loss + mAP@50 → krzywa w UI.
        if let Some(loss) = st.train_loss {
            repository::record_training_metric(run_id, st.epoch, "train_loss", loss)?;
        }
        if let Some(map50) = st.map50 {
            repository::record_training_metric(run_id, st.epoch, "map50", map50)?;
        }

        match st.status.as_str() {
            "running" => continue,
            "succeeded" => {
                let metrics_json = json!({
                    "train_loss": st.train_loss,
                    "map50": st.map50,
                    "map50_95": st.map50_95,
                    "epoch": st.epoch,
                    "total_epochs": st.total_epochs,
                    "checkpoint_path": st.artifact_path,
                    "variant": variant,
                    "class_names": class_names,
                })
                .to_string();
                let model_name = format!("rfdetr-{}", variant);
                let model_id = repository::insert_model(
                    project_id,
                    &model_name,
                    "rfdetr",
                    &format!("RF-DETR {}", variant),
                    &metrics_json,
                )?;
                repository::set_training_run_model(run_id, &model_id)?;
                repository::update_training_run_status(run_id, "succeeded")?;
                return Ok(());
            }
            "failed" => {
                let msg = st
                    .error
                    .unwrap_or_else(|| "rfdetr-training reported failure".to_string());
                anyhow::bail!("RF-DETR training failed: {}", msg);
            }
            other => anyhow::bail!("rfdetr-training unknown status '{}'", other),
        }
    }
}

/// Nazwa wstrzymanego splitu walidacyjnego co N-ty obraz (po posortowaniu po
/// file_name). 7 → ~15% obrazów trafia do valid/, reszta zostaje w train/.
const VALID_HOLDOUT_STRIDE: usize = 7;
/// Minimalna liczba obrazów w train/ wymagana do sensownego treningu.
const MIN_TRAIN_IMAGES: usize = 4;

/// Przygotowuje katalog treningowy z gwarantowanym splitem valid/.
///
/// Gdy `coco_path` ma już `valid/_annotations.coco.json` → zwraca go bez zmian.
/// Gdy ma TYLKO `train/` → tworzy efemeryczną kopię pod cache (train/ + valid/),
/// deterministycznie wstrzymując co `VALID_HOLDOUT_STRIDE`-ty obraz do valid/.
/// Oryginalny `coco_path` pozostaje NIETKNIĘTY (read-only).
fn prepare_dataset_with_valid(coco_path: &Path, run_id: &str) -> anyhow::Result<PreparedDataset> {
    let valid_annot = coco_path.join("valid").join("_annotations.coco.json");
    if valid_annot.is_file() {
        return Ok(PreparedDataset::Original(coco_path.to_path_buf()));
    }

    let train_dir = coco_path.join("train");
    let train_annot = train_dir.join("_annotations.coco.json");
    if !train_annot.is_file() {
        anyhow::bail!(
            "dataset COCO bez splitu valid/ ani train/ ({})",
            coco_path.display()
        );
    }

    let coco: serde_json::Value = serde_json::from_slice(&std::fs::read(&train_annot)?)
        .map_err(|e| anyhow::anyhow!("train/_annotations.coco.json niepoprawny: {}", e))?;
    let categories = coco.get("categories").cloned().unwrap_or(json!([]));
    let images = coco
        .get("images")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let annotations = coco
        .get("annotations")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    if images.len() < MIN_TRAIN_IMAGES {
        anyhow::bail!("zbiór za mały do treningu — dodaj więcej obrazów");
    }

    // Deterministyczny podział: sortujemy obrazy po file_name i co N-ty → valid/.
    // Brak RNG (Math::random/Date::now niedostępne) — stride daje powtarzalny split.
    let mut ordered: Vec<&serde_json::Value> = images.iter().collect();
    ordered.sort_by(|a, b| coco_image_file_name(a).cmp(&coco_image_file_name(b)));

    // RF-DETR trains with run_test=True, so it needs train/ valid/ AND test/. We do a
    // deterministic 3-way split by position mod stride: last slot → valid, second-last
    // → test, rest → train. No RNG (reproducible).
    let mut valid_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    let mut test_ids: std::collections::HashSet<i64> = std::collections::HashSet::new();
    for (idx, img) in ordered.iter().enumerate() {
        let Some(id) = img.get("id").and_then(|v| v.as_i64()) else {
            continue;
        };
        match idx % VALID_HOLDOUT_STRIDE {
            n if n == VALID_HOLDOUT_STRIDE - 1 => {
                valid_ids.insert(id);
            }
            n if n == VALID_HOLDOUT_STRIDE - 2 => {
                test_ids.insert(id);
            }
            _ => {}
        }
    }
    // Each eval split must hold >=1 image (small datasets); pull distinct images from
    // the end so valid/ and test/ never steal the same one.
    let rev_ids: Vec<i64> = ordered
        .iter()
        .rev()
        .filter_map(|img| img.get("id").and_then(|v| v.as_i64()))
        .collect();
    if valid_ids.is_empty() {
        if let Some(&id) = rev_ids.iter().find(|id| !test_ids.contains(id)) {
            valid_ids.insert(id);
        }
    }
    if test_ids.is_empty() {
        if let Some(&id) = rev_ids.iter().find(|id| !valid_ids.contains(id)) {
            test_ids.insert(id);
        }
    }
    let train_count = images.len().saturating_sub(valid_ids.len() + test_ids.len());
    if train_count < MIN_TRAIN_IMAGES {
        anyhow::bail!("zbiór za mały do treningu — dodaj więcej obrazów");
    }

    let dest = crate::paths::cache_dir()
        .join("ml-recog-train-split")
        .join(run_id);
    if dest.exists() {
        let _ = std::fs::remove_dir_all(&dest);
    }
    let dest_train = dest.join("train");
    let dest_valid = dest.join("valid");
    let dest_test = dest.join("test");
    std::fs::create_dir_all(&dest_train)?;
    std::fs::create_dir_all(&dest_valid)?;
    std::fs::create_dir_all(&dest_test)?;

    // Split images + annotations by image id (ids stay original — consistent within
    // each split). 0 = train, 1 = valid, 2 = test.
    let img_split = |id: i64| -> u8 {
        if valid_ids.contains(&id) {
            1
        } else if test_ids.contains(&id) {
            2
        } else {
            0
        }
    };
    let pick_imgs = |sel: u8| -> Vec<&serde_json::Value> {
        images
            .iter()
            .filter(|img| img.get("id").and_then(|v| v.as_i64()).map(img_split) == Some(sel))
            .collect()
    };
    let pick_annots = |sel: u8| -> Vec<&serde_json::Value> {
        annotations
            .iter()
            .filter(|a| a.get("image_id").and_then(|v| v.as_i64()).map(img_split) == Some(sel))
            .collect()
    };
    let (train_images, valid_images, test_images) = (pick_imgs(0), pick_imgs(1), pick_imgs(2));
    let (train_annots, valid_annots, test_annots) =
        (pick_annots(0), pick_annots(1), pick_annots(2));

    write_split_coco(&dest_train, &categories, &train_images, &train_annots)?;
    write_split_coco(&dest_valid, &categories, &valid_images, &valid_annots)?;
    write_split_coco(&dest_test, &categories, &test_images, &test_annots)?;

    // Kopiujemy (preferując hardlink) pliki obrazów do odpowiednich splitów.
    // Serwis czyta obrazy po file_name z katalogu danego splitu.
    copy_split_images(&train_dir, &dest_train, &train_images)?;
    copy_split_images(&train_dir, &dest_valid, &valid_images)?;
    copy_split_images(&train_dir, &dest_test, &test_images)?;

    Ok(PreparedDataset::Ephemeral(dest))
}

/// file_name obrazu z rekordu COCO (pusty string gdy brak — stabilne sortowanie).
fn coco_image_file_name(img: &serde_json::Value) -> String {
    img.get("file_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Zapisuje `_annotations.coco.json` dla jednego splitu: te same `categories`,
/// przypisane obrazy i tylko ich adnotacje.
fn write_split_coco(
    split_dir: &Path,
    categories: &serde_json::Value,
    images: &[&serde_json::Value],
    annotations: &[&serde_json::Value],
) -> anyhow::Result<()> {
    let doc = json!({
        "categories": categories,
        "images": images,
        "annotations": annotations,
    });
    std::fs::write(
        split_dir.join("_annotations.coco.json"),
        serde_json::to_vec(&doc)?,
    )?;
    Ok(())
}

/// Kopiuje pliki obrazów wymienione w `images` z `src_dir` do `dst_dir`. Preferuje
/// hardlink (zero dodatkowego miejsca); gdy się nie uda (inny FS) — kopiuje bajty.
fn copy_split_images(
    src_dir: &Path,
    dst_dir: &Path,
    images: &[&serde_json::Value],
) -> anyhow::Result<()> {
    for img in images {
        let Some(name) = img.get("file_name").and_then(|v| v.as_str()) else {
            continue;
        };
        // Tylko nazwa pliku — odcięcie ewentualnych komponentów ścieżki (zip slip).
        let Some(base) = Path::new(name).file_name() else {
            continue;
        };
        let src = src_dir.join(base);
        if !src.is_file() {
            anyhow::bail!("obraz datasetu nie istnieje: {}", src.display());
        }
        let dst = dst_dir.join(base);
        if std::fs::hard_link(&src, &dst).is_err() {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// Rozpakowuje zip COCO do `dest` (czyści wcześniejszą zawartość) i zwraca
/// nazwy klas wyciągnięte z pierwszego napotkanego `_annotations.coco.json`.
fn unpack_coco(zip_bytes: &[u8], dest: &Path) -> anyhow::Result<Vec<String>> {
    if dest.exists() {
        let _ = std::fs::remove_dir_all(dest);
    }
    std::fs::create_dir_all(dest)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| anyhow::anyhow!("dataset nie jest poprawnym zip COCO: {}", e))?;

    let mut class_names: Vec<String> = Vec::new();
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
        // Z pierwszego annotations.coco.json wyciągamy klasy (kolejność po id).
        if class_names.is_empty()
            && rel
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.ends_with("_annotations.coco.json"))
                .unwrap_or(false)
        {
            class_names = coco_class_names(&buf);
        }
        std::fs::write(&out_path, &buf)?;
    }
    Ok(class_names)
}

/// Czyta nazwy klas z pierwszego `*/_annotations.coco.json` w katalogu COCO.
fn coco_class_names_from_dir(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let annot = entry.path().join("_annotations.coco.json");
        if annot.is_file() {
            if let Ok(buf) = std::fs::read(&annot) {
                let names = coco_class_names(&buf);
                if !names.is_empty() {
                    return names;
                }
            }
        }
    }
    Vec::new()
}

/// Wyciąga nazwy klas z COCO json: `categories` posortowane po `id` rosnąco,
/// pomijając ewentualną kategorię tła o `id==0` (Roboflow/RF-DETR konwencja).
fn coco_class_names(json_bytes: &[u8]) -> Vec<String> {
    let Ok(value): Result<serde_json::Value, _> = serde_json::from_slice(json_bytes) else {
        return Vec::new();
    };
    let Some(cats) = value.get("categories").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    let mut cats: Vec<(i64, String)> = cats
        .iter()
        .filter_map(|c| {
            let id = c.get("id")?.as_i64()?;
            let name = c.get("name")?.as_str()?.to_string();
            Some((id, name))
        })
        .collect();
    cats.sort_by_key(|(id, _)| *id);
    let has_zero = cats.iter().any(|(id, _)| *id == 0);
    cats.into_iter()
        .filter(|(id, _)| !(has_zero && *id == 0))
        .map(|(_, name)| name)
        .collect()
}

// Rejestr jobów treningowych uruchomionych PRZEZ MESH na tym nodzie (odbiorca).
// Mapuje `run_id` (klucz inicjatora Node A) na lokalny job serwisu (base+job_id).
// In-memory — joby to byty runtime; artefakty żyją na dysku tego noda.
static MESH_JOBS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (String, String)>>> =
    std::sync::OnceLock::new();

fn mesh_jobs() -> &'static std::sync::Mutex<std::collections::HashMap<String, (String, String)>> {
    MESH_JOBS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

// Akumulacja chunków datasetu przyjmowanych przez mesh (B-side), per hash.
// Vec<Option<bytes>> indeksowany seq; po komplecie składamy zip i rozpakowujemy.
static MESH_DS_ACCUM: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, Vec<Option<Vec<u8>>>>>,
> = std::sync::OnceLock::new();

fn ds_accum() -> &'static std::sync::Mutex<std::collections::HashMap<String, Vec<Option<Vec<u8>>>>> {
    MESH_DS_ACCUM.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Katalog cache dla datasetu przyniesionego przez mesh (content-addr po hash).
/// Wspólny dla recognition (COCO dir) i LLM (blob JSONL) — adresowanie po hashu.
pub fn mesh_dataset_cache(hash: &str) -> std::path::PathBuf {
    crate::paths::cache_dir().join("ml-recog-mesh").join(hash)
}

/// Marker kompletnego rozpakowania (pisany dopiero PO udanym unzip). Dedup
/// sprawdza JEGO obecność, nie „jakikolwiek plik" — częściowe/zerwane
/// rozpakowanie nie zostanie uznane za gotowe i transfer się powtórzy.
fn cache_complete_marker(dir: &Path) -> std::path::PathBuf {
    dir.join(".complete")
}

/// Czy dataset pod hashem jest KOMPLETNIE zmaterializowany (marker `.complete`).
fn cache_complete(dir: &Path) -> bool {
    cache_complete_marker(dir).is_file()
}

/// sha256 surowych bajtów (content-hash blobu, np. datasetu JSONL dla LLM).
pub fn blob_content_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Pakuje pojedynczy blob do zip-a (jedna pozycja `name`) — do transferu mesh
/// blobów nie-katalogowych (dataset LLM). Współdzieli format z `zip_dir`.
pub fn zip_single_file(name: &str, bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    use std::io::Write;
    let mut cursor = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut cursor);
    let opts: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file(name, opts)?;
    zip.write_all(bytes)?;
    zip.finish()?;
    Ok(cursor.into_inner())
}

// Stała: brak postępu transferu przez tyle = błąd. NIE liczymy sztywnego deadline
// na cały transfer — duży dataset może iść długo; błędem jest dopiero STALL
// (prędkość spada do zera i nie rośnie).
const SYNC_STALL_TIMEOUT: Duration = Duration::from_secs(30);
const SYNC_CHUNK_BYTES: usize = 600 * 1024;
const SYNC_CHUNK_TIMEOUT_SECS: u64 = 25;

/// Postęp transferu datasetu A→B przez mesh (faza poprzedzająca trening zdalny).
/// Trzymane in-memory na A, odpytywane przez status handler dla paska postępu.
#[derive(Clone, Debug, Default)]
pub struct DatasetSyncProgress {
    pub phase: String, // "zipping" | "syncing" | "starting" | "training" | "error"
    pub bytes_sent: u64,
    pub bytes_total: u64,
    pub rate_bps: u64,
    pub error: Option<String>,
}

static RECOG_SYNC: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, DatasetSyncProgress>>,
> = std::sync::OnceLock::new();

fn recog_sync_map() -> &'static std::sync::Mutex<std::collections::HashMap<String, DatasetSyncProgress>>
{
    RECOG_SYNC.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn set_recog_sync(run_id: &str, p: DatasetSyncProgress) {
    if let Ok(mut m) = recog_sync_map().lock() {
        m.insert(run_id.to_string(), p);
    }
}

/// A-side: bieżący postęp transferu datasetu dla runu (None gdy brak/zakończony).
pub fn recog_sync_progress(run_id: &str) -> Option<DatasetSyncProgress> {
    recog_sync_map().lock().ok()?.get(run_id).cloned()
}

/// A-side: usuwa wpis postępu (po przejściu w fazę treningu na B).
pub fn clear_recog_sync(run_id: &str) {
    if let Ok(mut m) = recog_sync_map().lock() {
        m.remove(run_id);
    }
}

// Licznik kolejnych nieudanych pollingów statusu zdalnego runu (Node B nieosiągalny
// / zgubił job po restarcie). Po przekroczeniu progu run domykamy jako failed,
// zamiast trzymać go w „running" w nieskończoność.
static REMOTE_POLL_FAILS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, u32>>> =
    std::sync::OnceLock::new();
const REMOTE_POLL_FAIL_LIMIT: u32 = 15;

fn remote_poll_map() -> &'static std::sync::Mutex<std::collections::HashMap<String, u32>> {
    REMOTE_POLL_FAILS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Rejestruje wynik pollingu zdalnego statusu. `ok=true` zeruje licznik; `false`
/// inkrementuje i zwraca true gdy próg przekroczony (czas domknąć run jako failed).
pub fn note_remote_poll(run_id: &str, ok: bool) -> bool {
    let Ok(mut m) = remote_poll_map().lock() else { return false };
    if ok {
        m.remove(run_id);
        false
    } else {
        let n = m.entry(run_id.to_string()).or_insert(0);
        *n += 1;
        if *n >= REMOTE_POLL_FAIL_LIMIT {
            m.remove(run_id);
            true
        } else {
            false
        }
    }
}

/// A-side: transfer datasetu COCO przez mesh do węzła B, a po zmaterializowaniu
/// — start treningu (`MlTrainStart`). Biegnie w tle (task), żeby NIE blokować RPC
/// `train_start` na czas transferu. Postęp (bytes/total/rate) ląduje w
/// `RECOG_SYNC` i jest serwowany do UI jako pasek B/s. Błąd wykrywany przez STALL:
/// gdy przez `SYNC_STALL_TIMEOUT` nie przybędzie ani bajt (chunk wciąż nie-ACK),
/// transfer jest uznawany za zerwany. Pojedynczy chunk ma własny timeout i przy
/// błędzie jest ponawiany — to watchdog stallu (a nie pojedynczy timeout) decyduje
/// o porażce.
#[allow(clippy::too_many_arguments)]
pub fn spawn_mesh_dataset_push_and_train(
    iroh: std::sync::Arc<crate::mesh::iroh_manager::IrohMeshManager>,
    target: String,
    run_id: String,
    dataset_dir: String,
    dataset_hash: String,
    spec_json: String,
) {
    tokio::spawn(async move {
        let zipped = {
            set_recog_sync(
                &run_id,
                DatasetSyncProgress { phase: "zipping".into(), ..Default::default() },
            );
            let dir = std::path::PathBuf::from(&dataset_dir);
            tokio::task::spawn_blocking(move || zip_dir(&dir)).await
        };
        let result = match zipped {
            Ok(Ok(zip_bytes)) => {
                mesh_push_and_train(&iroh, &target, &run_id, zip_bytes, &dataset_hash, &spec_json)
                    .await
            }
            Ok(Err(e)) => Err(e),
            Err(e) => Err(anyhow::anyhow!("zip join: {}", e)),
        };
        if let Err(err) = result {
            tracing::warn!(run_id = %run_id, error = %err, "mesh dataset push/train failed");
            set_recog_sync(
                &run_id,
                DatasetSyncProgress {
                    phase: "error".into(),
                    error: Some(err.to_string()),
                    ..Default::default()
                },
            );
            let _ = repository::update_training_run_status(&run_id, "failed");
        }
    });
}

/// Wariant generyczny: transfer GOTOWEGO zip-a (np. spakowany blob JSONL dla LLM)
/// + start treningu na B. Współdzieli pasek postępu/stall i `MlTrainStart`.
pub fn spawn_mesh_push_and_train(
    iroh: std::sync::Arc<crate::mesh::iroh_manager::IrohMeshManager>,
    target: String,
    run_id: String,
    zip_bytes: Vec<u8>,
    dataset_hash: String,
    spec_json: String,
) {
    tokio::spawn(async move {
        if let Err(err) =
            mesh_push_and_train(&iroh, &target, &run_id, zip_bytes, &dataset_hash, &spec_json).await
        {
            tracing::warn!(run_id = %run_id, error = %err, "mesh blob push/train failed");
            set_recog_sync(
                &run_id,
                DatasetSyncProgress {
                    phase: "error".into(),
                    error: Some(err.to_string()),
                    ..Default::default()
                },
            );
            let _ = repository::update_training_run_status(&run_id, "failed");
        }
    });
}

async fn mesh_push_and_train(
    iroh: &crate::mesh::iroh_manager::IrohMeshManager,
    target: &str,
    run_id: &str,
    zip_bytes: Vec<u8>,
    dataset_hash: &str,
    spec_json: &str,
) -> anyhow::Result<()> {
    use tentaflow_protocol::mesh::{MeshCommandResponsePayload, MeshCommandType};
    use tokio::time::Instant;

    let total_bytes = zip_bytes.len() as u64;
    let total_chunks = zip_bytes.len().div_ceil(SYNC_CHUNK_BYTES).max(1) as u32;
    set_recog_sync(
        run_id,
        DatasetSyncProgress {
            phase: "syncing".into(),
            bytes_total: total_bytes,
            ..Default::default()
        },
    );

    let mut bytes_sent: u64 = 0;
    let mut last_advance = Instant::now();
    let mut window_start = Instant::now();
    let mut window_bytes: u64 = 0;
    let mut rate_bps: u64 = 0;
    let mut seq: u32 = 0;
    while seq < total_chunks {
        if last_advance.elapsed() > SYNC_STALL_TIMEOUT {
            anyhow::bail!(
                "transfer datasetu utknął — brak postępu przez {}s",
                SYNC_STALL_TIMEOUT.as_secs()
            );
        }
        let start = seq as usize * SYNC_CHUNK_BYTES;
        let end = (start + SYNC_CHUNK_BYTES).min(zip_bytes.len());
        let data_b64 = base64::engine::general_purpose::STANDARD.encode(&zip_bytes[start..end]);
        let chunk_cmd = MeshCommandType::MlDatasetChunk {
            dataset_hash: dataset_hash.to_string(),
            seq,
            total: total_chunks,
            data_b64,
        };
        match iroh
            .send_command_and_wait(target, chunk_cmd, SYNC_CHUNK_TIMEOUT_SECS)
            .await
        {
            Ok(cr) => {
                if let MeshCommandResponsePayload::MlDatasetChunkResult { have_already } =
                    cr.payload
                {
                    if have_already {
                        // Wspólny zasób / wcześniejszy transfer — B już ma dataset.
                        set_recog_sync(
                            run_id,
                            DatasetSyncProgress {
                                phase: "syncing".into(),
                                bytes_sent: total_bytes,
                                bytes_total: total_bytes,
                                rate_bps: 0,
                                error: None,
                            },
                        );
                        break;
                    }
                }
                let chunk_len = (end - start) as u64;
                bytes_sent += chunk_len;
                window_bytes += chunk_len;
                last_advance = Instant::now();
                let win = window_start.elapsed().as_secs_f64();
                if win >= 1.0 {
                    rate_bps = (window_bytes as f64 / win) as u64;
                    window_start = Instant::now();
                    window_bytes = 0;
                }
                set_recog_sync(
                    run_id,
                    DatasetSyncProgress {
                        phase: "syncing".into(),
                        bytes_sent,
                        bytes_total: total_bytes,
                        rate_bps,
                        error: None,
                    },
                );
                seq += 1;
            }
            Err(e) => {
                // Brak postępu — nie zwiększamy seq ani bytes_sent. Watchdog STALL
                // ubije transfer po SYNC_STALL_TIMEOUT, jeśli to się nie odblokuje.
                tracing::warn!(run_id = %run_id, seq, error = %e, "mesh chunk send failed, retry");
                set_recog_sync(
                    run_id,
                    DatasetSyncProgress {
                        phase: "syncing".into(),
                        bytes_sent,
                        bytes_total: total_bytes,
                        rate_bps: 0,
                        error: None,
                    },
                );
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }

    set_recog_sync(
        run_id,
        DatasetSyncProgress {
            phase: "starting".into(),
            bytes_sent: total_bytes,
            bytes_total: total_bytes,
            rate_bps: 0,
            error: None,
        },
    );
    let cmd = MeshCommandType::MlTrainStart {
        run_id: run_id.to_string(),
        spec_json: spec_json.to_string(),
    };
    let resp = iroh.send_command_and_wait(target, cmd, 30).await?;
    if !resp.ok {
        anyhow::bail!(resp
            .error
            .unwrap_or_else(|| "remote train start failed".to_string()));
    }
    set_recog_sync(
        run_id,
        DatasetSyncProgress {
            phase: "training".into(),
            bytes_sent: total_bytes,
            bytes_total: total_bytes,
            rate_bps: 0,
            error: None,
        },
    );
    Ok(())
}

/// B-side (odbiorca `MlDatasetChunk`): składa zip datasetu z chunków pod hashem;
/// po ostatnim chunku rozpakowuje do cache. Dedup: gdy cache hash już istnieje,
/// zwraca true (have_already) — nadawca przerywa transfer.
pub fn mesh_dataset_chunk(
    hash: &str,
    seq: u32,
    total: u32,
    data_b64: &str,
) -> anyhow::Result<bool> {
    let cache = mesh_dataset_cache(hash);
    // Dedup: dataset (po content-hashu) KOMPLETNIE zmaterializowany na tym węźle
    // (marker .complete) — wspólny zasób / wcześniejszy transfer. Częściowe
    // rozpakowanie nie ma markera → transfer się powtórzy (brak treningu na ułomku).
    if cache_complete(&cache) {
        return Ok(true);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.as_bytes())
        .map_err(|e| anyhow::anyhow!("chunk base64: {}", e))?;
    let mut map = ds_accum().lock().map_err(|_| anyhow::anyhow!("accum lock poisoned"))?;
    let slot = map.entry(hash.to_string()).or_insert_with(|| vec![None; total as usize]);
    if slot.len() != total as usize {
        *slot = vec![None; total as usize];
    }
    if (seq as usize) < slot.len() {
        slot[seq as usize] = Some(bytes);
    }
    // Komplet? — złóż zip i rozpakuj.
    if slot.iter().all(|c| c.is_some()) {
        let mut zip_bytes = Vec::new();
        for c in slot.iter() {
            zip_bytes.extend_from_slice(c.as_ref().unwrap());
        }
        map.remove(hash);
        drop(map);
        unpack_coco(&zip_bytes, &cache)?;
        // Marker kompletności PO udanym rozpakowaniu — dopiero teraz dedup
        // (cache_complete) uzna dataset za gotowy. Brak markera = niekompletny.
        let _ = std::fs::write(cache_complete_marker(&cache), b"ok");
    }
    Ok(false)
}

/// A-side: pakuje katalog datasetu COCO do zip-a w pamięci (do transferu mesh).
pub fn zip_dir(dir: &Path) -> anyhow::Result<Vec<u8>> {
    use std::io::Write;
    let mut cursor = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(&mut cursor);
    let opts: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
    fn add(
        zip: &mut zip::ZipWriter<&mut std::io::Cursor<Vec<u8>>>,
        opts: &zip::write::FileOptions<()>,
        base: &Path,
        cur: &Path,
    ) -> anyhow::Result<()> {
        for e in std::fs::read_dir(cur)? {
            let p = e?.path();
            let rel = p.strip_prefix(base)?.to_string_lossy().replace('\\', "/");
            if p.is_dir() {
                add(zip, opts, base, &p)?;
            } else {
                zip.start_file(rel, *opts)?;
                zip.write_all(&std::fs::read(&p)?)?;
            }
        }
        Ok(())
    }
    add(&mut zip, &opts, dir, dir)?;
    zip.finish()?;
    Ok(cursor.into_inner())
}

/// Fingerprint zawartości datasetu COCO: sha256 po posortowanych parach
/// (nazwa_splitu, bajty `_annotations.coco.json`). Służy do wykrycia WSPÓLNEGO
/// zasobu między nodami — gdy A i B widzą te same pliki (np. wspólny NAS), hash
/// się zgadza i transfer jest zbędny. Obrazów nie hashujemy (drogie) — adnotacje
/// COCO niosą file_name+rozmiary, więc jednoznacznie identyfikują zbiór.
pub fn coco_content_hash(dir: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for e in std::fs::read_dir(dir)? {
        let split = e?.path();
        let annot = split.join("_annotations.coco.json");
        if annot.is_file() {
            let name = split
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            entries.push((name, std::fs::read(&annot)?));
        }
    }
    if entries.is_empty() {
        anyhow::bail!("brak _annotations.coco.json w {}", dir.display());
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (name, bytes) in &entries {
        hasher.update(name.as_bytes());
        hasher.update([0u8]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// B-side (odbiorca komendy mesh `MlTrainStart`): startuje trening na LOKALNYM
/// serwisie tego noda wg `spec_json` (dataset_dir/class_names/variant/output_dir/
/// hyperparams) i zapamiętuje job pod `run_id`. Inicjator (A) odpytuje przez
/// `mesh_train_status`. Weryfikuje content-hash: dataset_dir na B musi być TYM
/// SAMYM zasobem co u A (te same pliki) — inaczej odmawia (transfer blobów przez
/// mesh dla NIE-wspólnego zasobu jest osobnym, jeszcze niezaimplementowanym
/// krokiem; NIE wolno trenować na cudzych/nie-tych danych).
pub async fn mesh_train_start(run_id: &str, spec_json: &str) -> anyhow::Result<()> {
    let spec: serde_json::Value = serde_json::from_str(spec_json)
        .map_err(|e| anyhow::anyhow!("spec_json invalid: {}", e))?;
    let dataset_dir_raw = spec
        .get("dataset_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("spec bez dataset_dir"))?;
    // "mesh:<hash>" → dataset przyniesiony przez mesh, w cache content-addr.
    let resolved;
    let dataset_dir: &str = if let Some(hash) = dataset_dir_raw.strip_prefix("mesh:") {
        let c = mesh_dataset_cache(hash);
        if !c.is_dir() {
            anyhow::bail!("dataset mesh nie zmaterializowany na tym nodzie (hash {})", hash);
        }
        resolved = c.to_string_lossy().to_string();
        &resolved
    } else {
        dataset_dir_raw
    };
    if !std::path::Path::new(dataset_dir).is_dir() {
        anyhow::bail!(
            "dataset niedostępny na tym nodzie ({}). Transfer datasetu przez mesh \
             (blob) dla nie-wspólnego zasobu nie jest jeszcze zaimplementowany — \
             użyj wspólnego zasobu (NAS) widocznego na obu nodach.",
            dataset_dir
        );
    }
    // Wykrycie wspólnego zasobu: hash zawartości MUSI zgadzać się z oczekiwanym
    // (policzonym przez A). Brak hasha w spec → starsza ścieżka, pomijamy.
    if let Some(expected) = spec.get("dataset_hash").and_then(|v| v.as_str()) {
        let actual = coco_content_hash(std::path::Path::new(dataset_dir))?;
        if actual != expected {
            anyhow::bail!(
                "dataset na tym nodzie to NIE ten sam zasób (hash mismatch: oczekiwano {}, jest {}). \
                 Transfer przez mesh dla nie-wspólnego zasobu niezaimplementowany.",
                &expected[..expected.len().min(12)],
                &actual[..actual.len().min(12)]
            );
        }
    }
    let class_names = spec.get("class_names").cloned().unwrap_or(json!([]));
    let variant = spec.get("variant").and_then(|v| v.as_str()).unwrap_or("base");
    let output_dir = spec
        .get("output_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("spec bez output_dir"))?;
    let hyperparams = spec.get("hyperparams").cloned().unwrap_or(json!({}));

    let endpoint = resolve_endpoint()?;
    let base = endpoint.trim_end_matches('/').to_string();
    let train_body = json!({
        "dataset_dir": dataset_dir,
        "class_names": class_names,
        "variant": variant,
        "output_dir": output_dir,
        "hyperparams": hyperparams,
    });
    let url = format!("{}/train", base);
    let job_id =
        tokio::task::spawn_blocking(move || post_train(&url, train_body)).await??;
    mesh_jobs()
        .lock()
        .map_err(|_| anyhow::anyhow!("mesh_jobs lock poisoned"))?
        .insert(run_id.to_string(), (base, job_id));
    Ok(())
}

/// B-side (odbiorca `MlTrainStatus`): odpytuje lokalny serwis o status joba
/// zmapowanego z `run_id` i zwraca surowy JSON statusu do inicjatora.
pub async fn mesh_train_status(run_id: &str) -> anyhow::Result<String> {
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

/// Detekcja na obrazie przez serwis rfdetr-training (`POST /detect`). Zwraca
/// (detections_json, width, height). `image_b64` to zakodowane base64 zdjęcie.
pub async fn run_detect(
    checkpoint_path: String,
    class_names: Vec<String>,
    variant: String,
    threshold: f64,
    image_b64: String,
) -> anyhow::Result<(String, u32, u32)> {
    let endpoint = resolve_endpoint()?;
    let url = format!("{}/detect", endpoint.trim_end_matches('/'));
    let body = json!({
        "checkpoint_path": checkpoint_path,
        "class_names": class_names,
        "variant": variant,
        "threshold": threshold,
        "image_b64": image_b64,
    });
    let value: serde_json::Value = tokio::task::spawn_blocking(move || {
        let http = http_agent();
        let mut resp = http
            .post(&url)
            .send_json(&body)
            .map_err(|e| anyhow::anyhow!("POST {} failed: {}", url, e))?;
        resp.body_mut()
            .read_json::<serde_json::Value>()
            .map_err(|e| anyhow::anyhow!("decode /detect response: {}", e))
    })
    .await??;

    let detections = value.get("detections").cloned().unwrap_or(json!([]));
    let width = value.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    let height = value.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    Ok((detections.to_string(), width, height))
}

fn resolve_endpoint() -> anyhow::Result<String> {
    let pool = crate::db::global_pool()
        .ok_or_else(|| anyhow::anyhow!("core service registry unavailable"))?;
    let conn = pool
        .read()
        .map_err(|_| anyhow::anyhow!("core db read"))?;
    let svcs =
        services_repo::services::list_by_category(&conn, "training", Some("rfdetr-training"))?;
    let svc = svcs.into_iter().next().ok_or_else(|| {
        anyhow::anyhow!("Serwis rfdetr-training niedostępny — wdróż go w Serwisach")
    })?;
    svc.endpoint_url
        .ok_or_else(|| anyhow::anyhow!("serwis rfdetr-training bez endpointu"))
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
    total_epochs: i64,
    #[serde(default)]
    train_loss: Option<f64>,
    #[serde(default)]
    map50: Option<f64>,
    #[serde(default)]
    map50_95: Option<f64>,
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

#[cfg(test)]
mod tests {
    use super::snap_resolution;

    #[test]
    fn snap_base_large_do_wielokrotnosci_56() {
        // base/large: DINOv2 wymaga wielokrotności 56
        assert_eq!(snap_resolution(560, "base"), 560); // już pasuje
        assert_eq!(snap_resolution(576, "base"), 560); // 576 bliżej 560 niż 616
        assert_eq!(snap_resolution(600, "large"), 616); // 600 bliżej 616 niż 560
        assert_eq!(snap_resolution(640, "base"), 616); // 640 bliżej 616 niż 672
    }

    #[test]
    fn snap_male_warianty_do_wielokrotnosci_32() {
        // nano/small/medium: windowed attention wymaga wielokrotności 32
        assert_eq!(snap_resolution(576, "small"), 576); // już pasuje
        assert_eq!(snap_resolution(560, "nano"), 576); // 560 → najbliższa *32
        assert_eq!(snap_resolution(600, "medium"), 608);
    }

    #[test]
    fn snap_respektuje_dolny_limit() {
        assert_eq!(snap_resolution(0, "base"), 224);
        assert_eq!(snap_resolution(100, "small"), 224);
    }
}
