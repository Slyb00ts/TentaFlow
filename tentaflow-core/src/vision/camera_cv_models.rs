// =============================================================================
// File: vision/camera_cv_models.rs — camera-CV model bundle download (Acme PoC)
// =============================================================================
//
// The always-on camera analysis pipeline (RF-DETR detector + state classifier +
// plate OCR) is exposed as three installable `vision` services so the models are
// installable from the catalog and visible to other mesh nodes — like any other
// engine. The runners in `vision/{detector_rfdetr,classifier_stan,ocr_plate}.rs`
// load lazily from `vision_models_dir()`, so the only deploy-time work is making
// sure the model files land there.
//
// Each bundle mixes two kinds of files:
//   * binary weights (`*.onnx`, ONNX external `*.onnx.data`) — large, downloaded
//     from the release URL declared in the manifest's `[[model_preset]] repo`,
//   * sidecar configs (`*-classes.json`, `*-config.json`) — tiny and stable, so
//     they are embedded in the binary and written out verbatim. This keeps the
//     pipeline working even when the release server only hosts the weights.
//
// The download base comes from the manifest (mesh-propagated), so pointing the
// install at a different release server is a manifest edit, not a code change.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::paths::vision_models_dir;
use crate::services::deploy::LogSink;
use crate::services::model_download::{download_with_progress, ProgressFn};

/// One file in a camera-CV bundle. `remote` set → fetched from `<base>/<name>`;
/// `embedded` set → written from the binary-embedded bytes (config sidecars).
struct CvFile {
    name: &'static str,
    remote: bool,
    embedded: Option<&'static str>,
}

struct CvBundle {
    engine_id: &'static str,
    files: &'static [CvFile],
}

const RFDETR_CLASSES: &str = include_str!("cv_assets/rfdetr-classes.json");
const STAN_CLASSES: &str = include_str!("cv_assets/stan-classes.json");
const PLATE_CONFIG: &str = include_str!("cv_assets/plate-ocr-config.json");

const BUNDLES: &[CvBundle] = &[
    CvBundle {
        engine_id: "rfdetr-adr",
        files: &[
            CvFile {
                // Burn weights artifact (architecture is compiled in; only weights
                // are distributed). Host the .bpk at the release URL.
                name: "rfdetr-base.bpk",
                remote: true,
                embedded: None,
            },
            CvFile {
                name: "rfdetr-classes.json",
                remote: false,
                embedded: Some(RFDETR_CLASSES),
            },
        ],
    },
    CvBundle {
        engine_id: "nalepka-stan",
        files: &[
            CvFile {
                name: "model_stan.bpk",
                remote: true,
                embedded: None,
            },
            CvFile {
                name: "stan-classes.json",
                remote: false,
                embedded: Some(STAN_CLASSES),
            },
        ],
    },
    CvBundle {
        engine_id: "plate-ocr",
        files: &[
            CvFile {
                name: "plate_ocr.bpk",
                remote: true,
                embedded: None,
            },
            CvFile {
                name: "plate-ocr-config.json",
                remote: false,
                embedded: Some(PLATE_CONFIG),
            },
        ],
    },
    CvBundle {
        // Depth Anything V2 Metric (Burn). Architecture is compiled in; only the
        // `.bpk` weights are distributed. Deploying `depth-native` (embedded) provisions
        // this file into `vision_models_dir()` and registers the service for mesh
        // discovery. Host the .bpk at the release URL.
        engine_id: "depth-native",
        files: &[CvFile {
            name: "depth-anything-v2-metric.bpk",
            remote: true,
            embedded: None,
        }],
    },
];

/// True when `engine_id` is one of the camera-CV pipeline services. Lets the
/// vision deploy path route these away from the tract `LoadedEngine` registry.
pub fn is_camera_cv_engine(engine_id: &str) -> bool {
    BUNDLES.iter().any(|b| b.engine_id == engine_id)
}

/// File names (weights + sidecars) belonging to a bundle. Used by the
/// `/models/manifest` + `/models/file` endpoints to scope what a named-bundle
/// token may serve.
pub fn bundle_file_names(engine_id: &str) -> Option<Vec<&'static str>> {
    bundle(engine_id).map(|b| b.files.iter().map(|f| f.name).collect())
}

/// PP-OCRv5 (onnx-ocr) bundle: `(nazwa, wymagany, opcjonalny_absolutny_url)`.
/// Gdy `abs_url` = `Some`, plik leci z tego URL-a zamiast `<base_url>/<name>` —
/// dict PP-OCRv5 trzymamy z kanonicznego repo PaddleOCR (modele det/rec hostuja
/// repo ONNX bez slownika). `cls` opcjonalny (404 nie przerywa deployu — silnik
/// pomija korekte kata), `det`/`rec`/`dict` wymagane.
const ONNX_OCR_FILES: &[(&str, bool, Option<&str>)] = &[
    ("ppocrv5_det.onnx", true, None),
    ("ppocrv5_rec.onnx", true, None),
    ("ppocrv5_cls.onnx", false, None),
    (
        "ppocrv5_dict.txt",
        true,
        Some("https://raw.githubusercontent.com/PaddlePaddle/PaddleOCR/main/ppocr/utils/dict/ppocrv5_dict.txt"),
    ),
];

/// Materializuje bundle PP-OCRv5 (onnx-ocr) do `vision_models_dir()`: pobiera
/// det/rec/cls/dict z `<base_url>/<file>`. `base_url` to manifestowy
/// `[[model_preset]] repo` (URL katalogu z plikami). Idempotentne — istniejace
/// pliki sa pomijane. Opcjonalny `cls` ktorego brak na serwerze (404) jest
/// tolerowany; brak wymaganego pliku to blad.
pub async fn ensure_onnx_ocr_bundle(base_url: &str, log_sink: Option<&LogSink>) -> Result<()> {
    let dir = vision_models_dir();
    std::fs::create_dir_all(&dir).map_err(|e| anyhow!("create {}: {}", dir.display(), e))?;

    let base = base_url.trim_end_matches('/');
    if base.is_empty() || !base.starts_with("http") {
        return Err(anyhow!(
            "onnx-ocr: brak URL bundla (manifest model_preset.repo musi byc URL-em http(s) do katalogu z plikami PP-OCRv5)"
        ));
    }

    for (name, required, abs_url) in ONNX_OCR_FILES {
        let dest = dir.join(name);
        if file_ok(&dest) {
            continue;
        }
        let url = match abs_url {
            Some(u) => (*u).to_string(),
            None => format!("{}/{}", base, name),
        };
        if let Some(s) = log_sink {
            s.phase("downloading-vision", &format!("Pobieram {}", name));
        }
        let progress: Option<ProgressFn> = log_sink
            .cloned()
            .map(|sink| progress_for_sink(sink, name.to_string()));

        match download_with_progress(&url, &dest, name, progress).await {
            Ok(_) => {
                if let Some(s) = log_sink {
                    s.info(&format!("onnx-ocr: {} pobrany", name));
                }
            }
            Err(e) => {
                if *required {
                    return Err(anyhow!("download {} from {}: {}", name, url, e));
                }
                if let Some(s) = log_sink {
                    s.info(&format!(
                        "onnx-ocr: opcjonalny {} niedostepny ({}) — pomijam korekte kata",
                        name, e
                    ));
                }
            }
        }
    }

    Ok(())
}

fn bundle(engine_id: &str) -> Option<&'static CvBundle> {
    BUNDLES.iter().find(|b| b.engine_id == engine_id)
}

fn file_ok(path: &PathBuf) -> bool {
    std::fs::metadata(path).map(|m| m.len() > 0).unwrap_or(false)
}

fn progress_for_sink(sink: LogSink, label: String) -> ProgressFn {
    Box::new(move |downloaded: u64, total: u64, _label: &str| {
        let pct: u8 = if total > 0 {
            (((downloaded as f64 / total as f64) * 100.0).clamp(0.0, 100.0)) as u8
        } else {
            0
        };
        let line = if total > 0 {
            format!("{}: {}/{} KB ({}%)", label, downloaded / 1024, total / 1024, pct)
        } else {
            format!("{}: {} KB", label, downloaded / 1024)
        };
        sink.progress("downloading-vision", pct, &line);
    })
}

/// Materializes the camera-CV bundle for `engine_id` into `vision_models_dir()`:
/// downloads the weights and writes the embedded config sidecars. `base_url`
/// is either the manifest `[[model_preset]] repo` (a release-dir URL serving
/// `<base>/<name>`) or — when the admin set `vision_bundle_base_url` — a
/// TentaFlow manifest URL containing `/models/manifest/`, in which case the
/// files are pulled through per-file signed URLs with sha256 verification.
/// `api_key` (manifest mode only) authenticates against an UNPAIRED serving
/// instance: it is sent as `Authorization: Bearer` on the manifest GET and on
/// every per-file GET (the serving node returns token-less file urls for
/// API-key manifests). Idempotent — files already present on disk are left
/// untouched.
pub async fn ensure_bundle(
    engine_id: &str,
    base_url: &str,
    api_key: Option<&str>,
    log_sink: Option<&LogSink>,
) -> Result<()> {
    let bundle = bundle(engine_id)
        .ok_or_else(|| anyhow!("'{}' is not a camera-CV engine", engine_id))?;

    let dir = vision_models_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow!("create {}: {}", dir.display(), e))?;

    let mut missing: Vec<&'static str> = Vec::new();
    for f in bundle.files {
        let dest = dir.join(f.name);

        if let Some(contents) = f.embedded {
            // Configs are authoritative in-binary; rewrite so a stale on-disk
            // copy never shadows a runner-format change shipped with the build.
            std::fs::write(&dest, contents)
                .map_err(|e| anyhow!("write {}: {}", dest.display(), e))?;
            continue;
        }

        if f.remote && !file_ok(&dest) {
            missing.push(f.name);
        }
    }

    if base_url.contains("/models/manifest/") {
        // Manifest mode carries per-file sha256 hashes, so EVERY required
        // file is verified against the manifest — already-present files with
        // a mismatched hash are deleted and re-downloaded. Deploy-time only,
        // so the extra hashing cost is acceptable.
        let required: Vec<&'static str> = bundle
            .files
            .iter()
            .filter(|f| f.remote)
            .map(|f| f.name)
            .collect();
        if required.is_empty() {
            return Ok(());
        }
        return download_from_bundle_manifest(engine_id, base_url, api_key, &required, log_sink)
            .await;
    }

    if missing.is_empty() {
        return Ok(());
    }

    let base = base_url.trim_end_matches('/');
    if base.is_empty() {
        return Err(anyhow!(
            "camera-CV '{}': no release URL configured (manifest model_preset.repo is empty)",
            engine_id
        ));
    }
    for name in missing {
        let dest = dir.join(name);
        let url = format!("{}/{}", base, name);

        if let Some(s) = log_sink {
            s.phase("downloading-vision", &format!("Pobieram {}", name));
        }
        let progress: Option<ProgressFn> = log_sink
            .cloned()
            .map(|sink| progress_for_sink(sink, name.to_string()));

        download_with_progress(&url, &dest, name, progress)
            .await
            .map_err(|e| anyhow!("download {} from {}: {}", name, url, e))?;

        if let Some(s) = log_sink {
            s.info(&format!("vision: {} pobrany", name));
        }
    }

    Ok(())
}

/// Ceiling for the manifest JSON body — the manifest lists at most a few
/// hundred entries, so anything past this is a misdirected URL, not a bundle.
const MANIFEST_BODY_LIMIT: u64 = 4 * 1024 * 1024;

/// Drop `?<query>` fragments from an error message so signed-URL tokens never
/// land in deploy logs. Everything from a `?` to the next whitespace/quote is
/// replaced with `?<redacted>`.
fn redact_query_strings(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    let mut skipping = false;
    for c in msg.chars() {
        if skipping {
            if c.is_whitespace() || c == '"' || c == '\'' || c == ')' {
                skipping = false;
                out.push(c);
            }
            continue;
        }
        if c == '?' {
            out.push_str("?<redacted>");
            skipping = true;
            continue;
        }
        out.push(c);
    }
    out
}

/// Pull the missing bundle files through another TentaFlow instance's
/// `/models/manifest/<ref>?token=...` endpoint: fetch the manifest JSON, then
/// download each needed file from its per-file signed URL and verify the
/// manifest sha256 (delete + fail on mismatch).
///
/// TLS posture matches `download_with_progress` (default reqwest trust roots,
/// no insecure bypass) — the serving instance must present a certificate the
/// pulling node trusts.
async fn download_from_bundle_manifest(
    engine_id: &str,
    manifest_url: &str,
    api_key: Option<&str>,
    needed: &[&'static str],
    log_sink: Option<&LogSink>,
) -> Result<()> {
    let base = reqwest::Url::parse(manifest_url)
        .map_err(|e| anyhow!("vision_bundle_base_url is not a valid URL: {}", e))?;

    // Bearer key for unpaired-instance pulls. It only ever accompanies
    // requests to the manifest origin — `resolve_manifest_file_url` pins every
    // file url to that exact scheme/host/port, so the key cannot leak to a
    // third-party host via a hostile manifest.
    let bearer = api_key.map(str::trim).filter(|k| !k.is_empty());

    // Policy::none — the manifest is a signed same-origin contract; following
    // a redirect would let a compromised serving node bounce the pull to an
    // arbitrary (possibly internal) destination. Matches the addon
    // `http.request` posture.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(600))
        .connect_timeout(std::time::Duration::from_secs(30))
        .user_agent(concat!("tentaflow/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| anyhow!("build HTTP client: {}", e))?;

    if let Some(s) = log_sink {
        s.phase("downloading-vision", "Fetching model bundle manifest");
    }
    // reqwest error Display embeds the request URL (incl. the token in the
    // query string) — redact before surfacing to deploy logs.
    let mut manifest_request = client.get(base.clone());
    if let Some(key) = bearer {
        manifest_request =
            manifest_request.header(reqwest::header::AUTHORIZATION, format!("Bearer {}", key));
    }
    let response = manifest_request
        .send()
        .await
        .map_err(|e| anyhow!("GET bundle manifest: {}", redact_query_strings(&e.to_string())))?;
    if response.status().is_redirection() {
        return Err(anyhow!(
            "bundle manifest responded with a redirect ({}) — redirects are not followed",
            response.status()
        ));
    }
    let response = response.error_for_status().map_err(|e| {
        anyhow!(
            "bundle manifest HTTP error: {}",
            redact_query_strings(&e.to_string())
        )
    })?;
    if response.content_length().unwrap_or(0) > MANIFEST_BODY_LIMIT {
        return Err(anyhow!("bundle manifest larger than {} bytes", MANIFEST_BODY_LIMIT));
    }
    let body = response.bytes().await.map_err(|e| {
        anyhow!(
            "read bundle manifest body: {}",
            redact_query_strings(&e.to_string())
        )
    })?;
    if body.len() as u64 > MANIFEST_BODY_LIMIT {
        return Err(anyhow!("bundle manifest larger than {} bytes", MANIFEST_BODY_LIMIT));
    }
    let manifest: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| anyhow!("parse bundle manifest JSON: {}", e))?;
    let entries = manifest
        .get("files")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("bundle manifest has no 'files' array"))?;

    let dir = vision_models_dir();
    for name in needed {
        let entry = entries
            .iter()
            .find(|e| e.get("name").and_then(|n| n.as_str()) == Some(*name))
            .ok_or_else(|| {
                anyhow!(
                    "camera-CV '{}': file '{}' missing from the remote bundle manifest \
                     (the serving node has not deployed it)",
                    engine_id,
                    name
                )
            })?;
        let rel_url = entry
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| anyhow!("manifest entry '{}' has no url", name))?;
        let expected_sha = entry
            .get("sha256")
            .and_then(|h| h.as_str())
            .map(str::to_ascii_lowercase)
            .filter(|h| h.len() == 64)
            .ok_or_else(|| anyhow!("manifest entry '{}' has no valid sha256", name))?;
        let file_url = resolve_manifest_file_url(&base, rel_url)
            .map_err(|why| anyhow!("manifest entry '{}': {}", name, why))?;

        let dest = dir.join(name);
        // Verify files already on disk against the manifest hash — a stale or
        // corrupted local copy is deleted and re-downloaded.
        if file_ok(&dest) {
            let actual_sha = crate::api::model_bundle::sha256_file_hex(&dest)
                .await
                .map_err(|e| anyhow!("hash {}: {}", dest.display(), e))?;
            if actual_sha == expected_sha {
                continue;
            }
            if let Some(s) = log_sink {
                s.info(&format!(
                    "vision: {} on disk mismatches the bundle manifest — re-downloading",
                    name
                ));
            }
            std::fs::remove_file(&dest)
                .map_err(|e| anyhow!("remove stale {}: {}", dest.display(), e))?;
        }

        if let Some(s) = log_sink {
            s.phase("downloading-vision", &format!("Pobieram {}", name));
        }
        let progress: Option<ProgressFn> = log_sink
            .cloned()
            .map(|sink| progress_for_sink(sink, name.to_string()));
        download_signed_file(&client, file_url, bearer, &dest, name, progress).await?;

        let actual_sha = crate::api::model_bundle::sha256_file_hex(&dest)
            .await
            .map_err(|e| anyhow!("hash {}: {}", dest.display(), e))?;
        if actual_sha != expected_sha {
            let _ = std::fs::remove_file(&dest);
            return Err(anyhow!(
                "camera-CV '{}': sha256 mismatch for '{}' (expected {}, got {}) — \
                 file deleted, deploy aborted",
                engine_id,
                name,
                expected_sha,
                actual_sha
            ));
        }
        if let Some(s) = log_sink {
            s.info(&format!("vision: {} pobrany (sha256 zweryfikowany)", name));
        }
    }

    Ok(())
}

/// SSRF guard for manifest file entries: the url MUST be an origin-relative
/// path under `/models/file/` (absolute and scheme-relative urls in a
/// compromised manifest must not steer the pull elsewhere), and the joined
/// result must stay on the manifest URL's exact scheme/host/port.
fn resolve_manifest_file_url(
    base: &reqwest::Url,
    rel_url: &str,
) -> std::result::Result<reqwest::Url, String> {
    if !rel_url.starts_with("/models/file/") {
        return Err("url is not an origin-relative /models/file/ path".to_string());
    }
    let file_url = base
        .join(rel_url)
        .map_err(|e| format!("resolve file url: {}", e))?;
    if file_url.scheme() != base.scheme()
        || file_url.host_str() != base.host_str()
        || file_url.port_or_known_default() != base.port_or_known_default()
    {
        return Err("url resolves off the manifest origin — rejected".to_string());
    }
    Ok(file_url)
}

/// Streaming download of one signed per-file URL. A dedicated path instead of
/// `model_download::download_with_progress` because this endpoint's trust
/// posture differs: redirects must NOT be followed (the shared no-redirect
/// `client` enforces it; a 3xx status is rejected explicitly since
/// `error_for_status` treats it as success) and error messages must have the
/// token-bearing query string redacted. Writes to `<dest>.partial` and
/// renames atomically, mirroring the shared downloader.
async fn download_signed_file(
    client: &reqwest::Client,
    url: reqwest::Url,
    bearer: Option<&str>,
    dest: &Path,
    label: &str,
    progress: Option<ProgressFn>,
) -> Result<()> {
    use std::io::Write;

    let mut request = client.get(url);
    if let Some(key) = bearer {
        request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {}", key));
    }
    let response = request
        .send()
        .await
        .map_err(|e| anyhow!("GET {}: {}", label, redact_query_strings(&e.to_string())))?;
    if response.status().is_redirection() {
        return Err(anyhow!(
            "download {} responded with a redirect ({}) — redirects are not followed",
            label,
            response.status()
        ));
    }
    let response = response.error_for_status().map_err(|e| {
        anyhow!(
            "download {} HTTP error: {}",
            label,
            redact_query_strings(&e.to_string())
        )
    })?;

    let total = response.content_length().unwrap_or(0);
    let partial = dest.with_extension(format!(
        "{}.partial",
        dest.extension().and_then(|s| s.to_str()).unwrap_or("tmp")
    ));
    let mut file = std::fs::File::create(&partial)
        .map_err(|e| anyhow!("create {}: {}", partial.display(), e))?;

    let mut downloaded: u64 = 0;
    let mut last_progress_bytes: u64 = 0;
    const PROGRESS_INTERVAL_BYTES: u64 = 256 * 1024;

    let mut stream = response.bytes_stream();
    use futures::StreamExt;
    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res
            .map_err(|e| anyhow!("stream {}: {}", label, redact_query_strings(&e.to_string())))?;
        file.write_all(&chunk)
            .map_err(|e| anyhow!("write {}: {}", partial.display(), e))?;
        downloaded += chunk.len() as u64;
        if downloaded - last_progress_bytes >= PROGRESS_INTERVAL_BYTES {
            if let Some(ref cb) = progress {
                cb(downloaded, total, label);
            }
            last_progress_bytes = downloaded;
        }
    }
    file.flush()
        .map_err(|e| anyhow!("flush {}: {}", partial.display(), e))?;
    drop(file);

    std::fs::rename(&partial, dest)
        .map_err(|e| anyhow!("rename {} -> {}: {}", partial.display(), dest.display(), e))?;
    if let Some(ref cb) = progress {
        cb(downloaded, downloaded, label);
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// Custom import (unpaired instance → remote /models/manifest with API key)
// -----------------------------------------------------------------------------

/// No-redirect, timeout-bounded HTTP client shared by every model-bundle pull.
/// `Policy::none` is mandatory: a compromised serving node must not be able to
/// bounce the pull (with its Bearer key) to an arbitrary destination.
fn bundle_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(600))
        .connect_timeout(std::time::Duration::from_secs(30))
        .user_agent(concat!("tentaflow/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| anyhow!("build HTTP client: {}", e))
}

/// GET a remote `/models/manifest/<ref>` with an API key and return the parsed
/// JSON. Server-side only (the browser never fetches an arbitrary instance):
/// no-redirect client, body-size cap, query-string redaction on errors. The
/// key travels ONLY to the manifest origin. `api_key` empty → no auth header
/// (lets a signed-URL manifest also be previewed).
pub async fn fetch_custom_manifest_json(
    manifest_url: &str,
    api_key: &str,
) -> Result<serde_json::Value> {
    if !manifest_url.contains("/models/manifest/") {
        return Err(anyhow!(
            "URL musi wskazywać na /models/manifest/<ref> innej instancji TentaFlow"
        ));
    }
    let base = reqwest::Url::parse(manifest_url)
        .map_err(|e| anyhow!("nieprawidłowy URL manifestu: {}", e))?;
    if base.scheme() != "https" {
        return Err(anyhow!("URL manifestu musi używać https"));
    }
    let bearer = api_key.trim();
    let client = bundle_http_client()?;
    let mut request = client.get(base.clone());
    if !bearer.is_empty() {
        request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {}", bearer));
    }
    let response = request
        .send()
        .await
        .map_err(|e| anyhow!("GET manifestu: {}", redact_query_strings(&e.to_string())))?;
    if response.status().is_redirection() {
        return Err(anyhow!(
            "manifest odpowiedział przekierowaniem ({}) — przekierowania nie są śledzone",
            response.status()
        ));
    }
    let response = response
        .error_for_status()
        .map_err(|e| anyhow!("błąd HTTP manifestu: {}", redact_query_strings(&e.to_string())))?;
    if response.content_length().unwrap_or(0) > MANIFEST_BODY_LIMIT {
        return Err(anyhow!("manifest większy niż {} bajtów", MANIFEST_BODY_LIMIT));
    }
    let body = response
        .bytes()
        .await
        .map_err(|e| anyhow!("odczyt manifestu: {}", redact_query_strings(&e.to_string())))?;
    if body.len() as u64 > MANIFEST_BODY_LIMIT {
        return Err(anyhow!("manifest większy niż {} bajtów", MANIFEST_BODY_LIMIT));
    }
    serde_json::from_slice(&body).map_err(|e| anyhow!("parsowanie JSON manifestu: {}", e))
}

/// Registry metadata + sha-verified on-disk files after a successful custom
/// import — everything the caller needs to insert the `vision_models` row.
pub struct CustomImport {
    pub model_name: String,
    pub op: String,
    pub file_name: String,
    pub classes_json: String,
    pub preprocess_json: String,
    pub output_contract: String,
    pub default_threshold: Option<f64>,
    /// Files written into `vision_models_dir()` — the caller removes them if
    /// the registry insert is refused (so a failed import leaves no orphans).
    pub written_files: Vec<PathBuf>,
}

/// Import ONE registry model from a remote instance: re-fetch the manifest with
/// the key, require its single-model `model` metadata, download every listed
/// file (Bearer per file, origin-pinned), verify sha256, and return the row
/// metadata. Files land in `vision_models_dir()`; the caller registers the row
/// and, on refusal, deletes `written_files`.
pub async fn import_custom_model(
    manifest_url: &str,
    api_key: &str,
    model_name: &str,
    log_sink: Option<&LogSink>,
) -> Result<CustomImport> {
    let base = reqwest::Url::parse(manifest_url)
        .map_err(|e| anyhow!("nieprawidłowy URL manifestu: {}", e))?;
    let manifest = fetch_custom_manifest_json(manifest_url, api_key).await?;

    let model = manifest
        .get("model")
        .and_then(|m| m.as_object())
        .ok_or_else(|| {
            anyhow!("manifest nie opisuje pojedynczego modelu rejestru (brak pola 'model')")
        })?;
    let remote_name = model
        .get("model_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if remote_name != model_name {
        return Err(anyhow!(
            "manifest opisuje model '{}', a zażądano '{}'",
            remote_name,
            model_name
        ));
    }
    let field = |key: &str| -> Result<String> {
        model
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("metadane modelu bez pola '{}'", key))
    };
    let op = field("op")?;
    let file_name = field("file_name")?;
    let classes_json = field("classes_json")?;
    let preprocess_json = field("preprocess_json")?;
    let output_contract = field("output_contract")?;
    let default_threshold = model.get("default_threshold").and_then(|v| v.as_f64());

    let entries = manifest
        .get("files")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("manifest bez tablicy 'files'"))?;

    let bearer = api_key.trim();
    let bearer_opt = (!bearer.is_empty()).then_some(bearer);
    let client = bundle_http_client()?;
    let dir = vision_models_dir();
    std::fs::create_dir_all(&dir).map_err(|e| anyhow!("create {}: {}", dir.display(), e))?;

    let mut written_files: Vec<PathBuf> = Vec::new();
    let cleanup = |files: &[PathBuf]| {
        for f in files {
            let _ = std::fs::remove_file(f);
        }
    };

    for entry in entries {
        let name = entry.get("name").and_then(|n| n.as_str()).unwrap_or_default();
        if name.is_empty() {
            cleanup(&written_files);
            return Err(anyhow!("wpis manifestu bez nazwy pliku"));
        }
        let expected_sha = entry
            .get("sha256")
            .and_then(|h| h.as_str())
            .map(str::to_ascii_lowercase)
            .filter(|h| h.len() == 64)
            .ok_or_else(|| {
                cleanup(&written_files);
                anyhow!("wpis '{}' bez poprawnego sha256", name)
            })?;
        let rel_url = entry.get("url").and_then(|u| u.as_str()).ok_or_else(|| {
            cleanup(&written_files);
            anyhow!("wpis '{}' bez url", name)
        })?;
        let file_url = resolve_manifest_file_url(&base, rel_url).map_err(|why| {
            cleanup(&written_files);
            anyhow!("wpis '{}': {}", name, why)
        })?;
        let dest = dir.join(name);
        if let Some(s) = log_sink {
            s.phase("downloading-vision", &format!("Pobieram {}", name));
        }
        let progress: Option<ProgressFn> = log_sink
            .cloned()
            .map(|sink| progress_for_sink(sink, name.to_string()));
        if let Err(e) =
            download_signed_file(&client, file_url, bearer_opt, &dest, name, progress).await
        {
            cleanup(&written_files);
            return Err(e);
        }
        written_files.push(dest.clone());
        let actual_sha = crate::api::model_bundle::sha256_file_hex(&dest)
            .await
            .map_err(|e| anyhow!("hash {}: {}", dest.display(), e))?;
        if actual_sha != expected_sha {
            cleanup(&written_files);
            return Err(anyhow!(
                "sha256 niezgodny dla '{}' (oczekiwano {}, jest {}) — import przerwany",
                name,
                expected_sha,
                actual_sha
            ));
        }
        if let Some(s) = log_sink {
            s.info(&format!("vision: {} pobrany (sha256 zweryfikowany)", name));
        }
    }

    Ok(CustomImport {
        model_name: model_name.to_string(),
        op,
        file_name,
        classes_json,
        preprocess_json,
        output_contract,
        default_threshold,
        written_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> reqwest::Url {
        reqwest::Url::parse("https://node-a.example:8090/models/manifest/vision-all?token=SECRET")
            .unwrap()
    }

    #[test]
    fn manifest_file_url_accepts_origin_relative_models_file_path() {
        let url = resolve_manifest_file_url(
            &base(),
            "/models/file/vision-all/rfdetr-base.bpk?token=T&exp=1&ref=vision-all%2Frfdetr-base.bpk",
        )
        .expect("valid entry url");
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("node-a.example"));
        assert_eq!(url.port_or_known_default(), Some(8090));
        assert!(url.path().starts_with("/models/file/vision-all/"));
    }

    #[test]
    fn manifest_file_url_rejects_absolute_and_scheme_relative() {
        assert!(resolve_manifest_file_url(&base(), "https://internal/steal").is_err());
        assert!(resolve_manifest_file_url(&base(), "//internal/steal").is_err());
        assert!(resolve_manifest_file_url(&base(), "http://node-a.example:8090/models/file/x/y")
            .is_err());
        assert!(resolve_manifest_file_url(&base(), "relative/path").is_err());
        assert!(resolve_manifest_file_url(&base(), "/frames/somewhere").is_err());
    }

    #[test]
    fn redact_query_strings_strips_tokens() {
        let msg = "HTTP error from https://h:8090/models/file/a/b.bpk?token=SECRET&exp=1 (status)";
        let out = redact_query_strings(msg);
        assert!(!out.contains("SECRET"));
        assert!(out.contains("/models/file/a/b.bpk?<redacted>"));
        assert!(out.ends_with("(status)"));
    }

    #[test]
    fn redact_query_strings_handles_url_at_end() {
        let out = redact_query_strings("GET https://h/x?token=SECRET");
        assert_eq!(out, "GET https://h/x?<redacted>");
    }
}
