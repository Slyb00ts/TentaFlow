// =============================================================================
// File: vision/camera_cv_models.rs — camera-CV model bundle download (ADR PoC)
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

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::api::model_bundle::validate_file_name;
use crate::paths::vision_models_dir;
use crate::services::deploy::LogSink;
use crate::services::model_download::{download_with_progress, ProgressFn};

/// One file in a camera-CV bundle. `remote` set → fetched from `<base>/<name>`;
/// `embedded` set → written from the binary-embedded bytes (config sidecars).
/// `ort_only` marks artifacts (the ORT `.onnx` graphs) that ONLY the
/// `vision-ort` build loads — a pure-Burn deploy must not require them.
struct CvFile {
    name: &'static str,
    remote: bool,
    embedded: Option<&'static str>,
    ort_only: bool,
}

struct CvBundle {
    engine_id: &'static str,
    files: &'static [CvFile],
}

impl CvBundle {
    /// Files that apply to THIS build. On a pure-Burn build the ort-only `.onnx`
    /// artifacts are dropped, so `ensure_bundle` never demands them from the
    /// release URL / remote manifest (they are an ORT-only rollout dependency).
    /// `cfg!(...)` is a plain runtime bool, so one const array serves both builds.
    fn effective_files(&self) -> Vec<&'static CvFile> {
        let ort = cfg!(feature = "vision-ort");
        self.files
            .iter()
            .filter(|f| ort || !f.ort_only)
            .collect()
    }
}

const RFDETR_CLASSES: &str = include_str!("cv_assets/rfdetr-classes.json");
const STAN_CLASSES: &str = include_str!("cv_assets/stan-classes.json");
const PLATE_CONFIG: &str = include_str!("cv_assets/plate-ocr-config.json");
const ADR_OCR_ALPHABET: &str = include_str!("cv_assets/adr-ocr-alphabet.txt");

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
                ort_only: false,
            },
            CvFile {
                // ONNX graph for the ORT/TensorRT session pool. Only the vision-ort
                // build loads it — `ort_only` keeps it out of a pure-Burn
                // deploy's required set (see `CvBundle::effective_files`).
                name: "rfdetr-base.onnx",
                remote: true,
                embedded: None,
                ort_only: true,
            },
            CvFile {
                name: "rfdetr-classes.json",
                remote: false,
                embedded: Some(RFDETR_CLASSES),
                ort_only: false,
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
                ort_only: false,
            },
            CvFile {
                // ONNX graph + its external-weights sidecar for the ort session
                // pool. Supertonic-only (see rfdetr-base.onnx).
                name: "model_stan.onnx",
                remote: true,
                embedded: None,
                ort_only: true,
            },
            CvFile {
                name: "model_stan.onnx.data",
                remote: true,
                embedded: None,
                ort_only: true,
            },
            CvFile {
                name: "stan-classes.json",
                remote: false,
                embedded: Some(STAN_CLASSES),
                ort_only: false,
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
                ort_only: false,
            },
            CvFile {
                // ONNX graph for the ort session pool. Supertonic-only (see rfdetr).
                name: "plate_ocr.onnx",
                remote: true,
                embedded: None,
                ort_only: true,
            },
            CvFile {
                name: "plate-ocr-config.json",
                remote: false,
                embedded: Some(PLATE_CONFIG),
                ort_only: false,
            },
            CvFile {
                // Nasz wytrenowany CRNN do numerów ADR (~4 MB). GŁÓWNY czytnik
                // trybu ADR (`vision::adr_ocr`); dystrybuowany z bundlem OCR do
                // pozostałych node'ów przez release URL, jak plate/rfdetr/stan.
                name: "adr_ocr.onnx",
                remote: true,
                embedded: None,
                ort_only: false,
            },
            CvFile {
                // Alfabet klas ADR OCR (`0123456789`) — mały i stabilny, więc
                // wbudowany w binarkę i zapisywany verbatim, jak sidecar-konfigi.
                name: "adr_ocr_alphabet.txt",
                remote: false,
                embedded: Some(ADR_OCR_ALPHABET),
                ort_only: false,
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
            ort_only: false,
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
    bundle(engine_id).map(|b| b.effective_files().iter().map(|f| f.name).collect())
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
    std::fs::metadata(path)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

fn progress_for_sink(sink: LogSink, label: String) -> ProgressFn {
    Box::new(move |downloaded: u64, total: u64, _label: &str| {
        let pct: u8 = if total > 0 {
            (((downloaded as f64 / total as f64) * 100.0).clamp(0.0, 100.0)) as u8
        } else {
            0
        };
        let line = if total > 0 {
            format!(
                "{}: {}/{} KB ({}%)",
                label,
                downloaded / 1024,
                total / 1024,
                pct
            )
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
    let bundle =
        bundle(engine_id).ok_or_else(|| anyhow!("'{}' is not a camera-CV engine", engine_id))?;

    let dir = vision_models_dir();
    std::fs::create_dir_all(&dir).map_err(|e| anyhow!("create {}: {}", dir.display(), e))?;

    let effective = bundle.effective_files();
    let mut missing: Vec<&'static str> = Vec::new();
    for f in &effective {
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
        let required: Vec<&'static str> = effective
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

/// Cumulative hard ceiling across every file pulled in one bundle/import. Real
/// bundles are a handful of files topping out near ~126 MB each; 2 GiB is far
/// past any legitimate deploy and bounds a hostile manifest declaring giant
/// (or size-less) files from filling the disk.
const TOTAL_IMPORT_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

/// Setting key: when truthy, model-bundle pulls may reach loopback/private/
/// link-local hosts. Absent/false → deny (the safe default).
const ALLOW_PRIVATE_HOSTS_SETTING: &str = "vision_bundle_allow_private_hosts";
const ALLOW_INVALID_TLS_SETTING: &str = "vision_bundle_allow_invalid_tls";

/// Read a boolean global setting (accepts `1`/`true`/`yes`), default false.
fn bundle_bool_setting(key: &str) -> bool {
    crate::db::global_pool()
        .and_then(|pool| crate::db::repository::get_setting(&pool, key).ok().flatten())
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "1" || v == "true" || v == "yes"
        })
        .unwrap_or(false)
}

/// Whether the admin opted into pulling from private/LAN hosts. Deploy-time
/// only, read from the global settings row.
fn allow_private_bundle_hosts() -> bool {
    bundle_bool_setting(ALLOW_PRIVATE_HOSTS_SETTING)
}

/// Whether the admin opted into accepting self-signed / otherwise-invalid TLS
/// certificates when pulling a bundle or shared project. Cross-instance and
/// intra-fleet nodes commonly serve self-signed certs; this opt-in (default
/// false → full validation) mirrors the `curl -k` intra-fleet trust the deploy
/// runbook assumes. It weakens transport authentication (MITM), so it is
/// deliberately off unless an admin sets it for a trusted fleet.
fn allow_invalid_bundle_tls() -> bool {
    bundle_bool_setting(ALLOW_INVALID_TLS_SETTING)
}

/// Resolve `base`'s host to socket addresses and reject non-public IPs unless
/// the admin opted in. Returns the vetted addresses so the HTTP client can be
/// PINNED to them, closing the DNS-rebind window between this check and connect.
///
/// Trust model: this is an admin/PowerUser deploy-time feature with an
/// explicitly pasted URL — the admin chose the host. We still default-deny
/// loopback/private/link-local so a hostile manifest host (or a rebinding DNS
/// record) cannot steer the Bearer key at an internal service without an
/// explicit `vision_bundle_allow_private_hosts` opt-in (e.g. intra-LAN
/// instance-to-instance pulls). Reuses the web-research SSRF IP classifier.
fn vet_bundle_host(base: &reqwest::Url) -> Result<Vec<SocketAddr>> {
    let host = base
        .host_str()
        .ok_or_else(|| anyhow!("bundle URL has no host"))?;
    let port = base
        .port_or_known_default()
        .ok_or_else(|| anyhow!("bundle URL has no port"))?;
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| anyhow!("resolve bundle host: {}", e))?
        .collect();
    if addrs.is_empty() {
        return Err(anyhow!("bundle host resolved to no addresses"));
    }
    if !allow_private_bundle_hosts()
        && addrs
            .iter()
            .any(|a| !crate::web_research::security::is_public_ip(a.ip()))
    {
        return Err(anyhow!(
            "bundle host resolves to a private/loopback address; set \
             '{}' = true to allow intra-LAN instance pulls",
            ALLOW_PRIVATE_HOSTS_SETTING
        ));
    }
    Ok(addrs)
}

/// Resolve the on-disk destination for a manifest entry `name` and assert it
/// stays directly inside `vision_models_dir()`. `validate_file_name` already
/// rejects separators and `..`, so `name` cannot traverse — this canonicalizes
/// the parent and compares it to the models dir anyway, matching the
/// O_NOFOLLOW/containment posture of the `/models/file` endpoint (defense in
/// depth against a future allowlist gap).
fn contained_model_dest(dir: &Path, name: &str) -> Result<PathBuf> {
    let dest = dir.join(name);
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow!("destination '{}' has no parent", name))?;
    let canon_parent = std::fs::canonicalize(parent)
        .map_err(|e| anyhow!("canonicalize destination parent for '{}': {}", name, e))?;
    let canon_dir =
        std::fs::canonicalize(dir).map_err(|e| anyhow!("canonicalize vision models dir: {}", e))?;
    if canon_parent != canon_dir {
        return Err(anyhow!(
            "destination '{}' escapes the vision models directory",
            name
        ));
    }
    Ok(dest)
}

/// Per-file byte ceiling enforced during a streaming download. The declared
/// manifest `size` (when > 0) is the primary gate; a size-less entry falls back
/// to the remaining cumulative budget. Either way the file may not push the
/// running import total past `TOTAL_IMPORT_LIMIT`.
fn per_file_ceiling(declared_size: u64, cumulative: u64) -> std::result::Result<u64, String> {
    let remaining = TOTAL_IMPORT_LIMIT
        .checked_sub(cumulative)
        .ok_or_else(|| format!("cumulative import exceeded {} bytes", TOTAL_IMPORT_LIMIT))?;
    if remaining == 0 {
        return Err(format!(
            "cumulative import reached {} bytes",
            TOTAL_IMPORT_LIMIT
        ));
    }
    if declared_size > remaining {
        return Err(format!(
            "declared size {} would exceed the {} byte import ceiling",
            declared_size, TOTAL_IMPORT_LIMIT
        ));
    }
    Ok(if declared_size > 0 {
        declared_size
    } else {
        remaining
    })
}

/// Read a manifest response body with a hard cap. Content-Length MUST be
/// present and within the cap, and the body is streamed with a running counter
/// so a lying (or absent-then-huge) body is aborted before it is buffered.
pub(crate) async fn read_capped_manifest_body(response: reqwest::Response) -> Result<Vec<u8>> {
    match response.content_length() {
        Some(len) if len <= MANIFEST_BODY_LIMIT => {}
        Some(_) => {
            return Err(anyhow!(
                "bundle manifest larger than {} bytes",
                MANIFEST_BODY_LIMIT
            ))
        }
        None => {
            return Err(anyhow!(
                "bundle manifest has no Content-Length — refusing to buffer an unbounded body"
            ))
        }
    }
    use futures::StreamExt;
    let mut stream = response.bytes_stream();
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.map_err(|e| {
            anyhow!(
                "read bundle manifest body: {}",
                redact_query_strings(&e.to_string())
            )
        })?;
        if body.len() as u64 + chunk.len() as u64 > MANIFEST_BODY_LIMIT {
            return Err(anyhow!(
                "bundle manifest exceeded {} bytes mid-stream",
                MANIFEST_BODY_LIMIT
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Drop `?<query>` fragments from an error message so signed-URL tokens never
/// land in deploy logs. Everything from a `?` to the next whitespace/quote is
/// replaced with `?<redacted>`.
pub(crate) fn redact_query_strings(msg: &str) -> String {
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

    // Policy::none + DNS-pinned to the vetted host: the manifest is a signed
    // same-origin contract; following a redirect or a rebound DNS record would
    // let a compromised serving node bounce the pull (with its Bearer key) to
    // an arbitrary (possibly internal) destination. Matches the addon
    // `http.request` posture.
    let client = bundle_http_client(&base, Some(std::time::Duration::from_secs(600)))?;

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
    let response = manifest_request.send().await.map_err(|e| {
        anyhow!(
            "GET bundle manifest: {}",
            redact_query_strings(&e.to_string())
        )
    })?;
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
    let body = read_capped_manifest_body(response).await?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| anyhow!("parse bundle manifest JSON: {}", e))?;
    let entries = manifest
        .get("files")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("bundle manifest has no 'files' array"))?;

    let dir = vision_models_dir();
    let mut cumulative: u64 = 0;
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
        let declared_size = entry.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
        let file_url = resolve_manifest_file_url(&base, rel_url)
            .map_err(|why| anyhow!("manifest entry '{}': {}", name, why))?;
        let max_bytes = per_file_ceiling(declared_size, cumulative)
            .map_err(|why| anyhow!("manifest entry '{}': {}", name, why))?;

        let dest = contained_model_dest(&dir, name)?;
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
        let written =
            download_signed_file(&client, file_url, bearer, &dest, name, max_bytes, progress)
                .await?;
        cumulative += written;

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
///
/// `max_bytes` is a hard streaming ceiling: if the body pushes past it the
/// partial file is deleted and the download fails, so a hostile server cannot
/// stream an unbounded body. Returns the number of bytes written.
async fn download_signed_file(
    client: &reqwest::Client,
    url: reqwest::Url,
    bearer: Option<&str>,
    dest: &Path,
    label: &str,
    max_bytes: u64,
    progress: Option<ProgressFn>,
) -> Result<u64> {
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

    // A lying Content-Length past the ceiling is rejected before writing a byte.
    if response.content_length().unwrap_or(0) > max_bytes {
        return Err(anyhow!(
            "download {} exceeds the {} byte ceiling (Content-Length)",
            label,
            max_bytes
        ));
    }
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
        downloaded += chunk.len() as u64;
        if downloaded > max_bytes {
            drop(file);
            let _ = std::fs::remove_file(&partial);
            return Err(anyhow!(
                "download {} exceeded the {} byte ceiling mid-stream — partial deleted",
                label,
                max_bytes
            ));
        }
        file.write_all(&chunk)
            .map_err(|e| anyhow!("write {}: {}", partial.display(), e))?;
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
    Ok(downloaded)
}

// -----------------------------------------------------------------------------
// Custom import (unpaired instance → remote /models/manifest with API key)
// -----------------------------------------------------------------------------

/// No-redirect, timeout-bounded HTTP client shared by every model-bundle pull,
/// DNS-pinned to `base`'s vetted host addresses. `Policy::none` is mandatory: a
/// compromised serving node must not be able to bounce the pull (with its
/// Bearer key) to an arbitrary destination. Pinning the resolved address closes
/// the DNS-rebind window between the SSRF check and connect.
/// `total_timeout` bounds the WHOLE request incl. body — right for small manifests
/// and model files, but WRONG for multi-GB archives: a slow-but-steady 10 GB pull
/// legitimately outlasts any fixed cap and would be killed mid-stream. Such callers
/// pass `None` and enforce a no-progress (stall) timeout on the body instead, so a
/// download only fails when the peer sends NOTHING for a while, never for being slow.
pub(crate) fn bundle_http_client(
    base: &reqwest::Url,
    total_timeout: Option<std::time::Duration>,
) -> Result<reqwest::Client> {
    let host = base
        .host_str()
        .ok_or_else(|| anyhow!("bundle URL has no host"))?;
    let addrs = vet_bundle_host(base)?;
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(30))
        .user_agent(concat!("tentaflow/", env!("CARGO_PKG_VERSION")))
        .resolve_to_addrs(host, &addrs);
    if let Some(t) = total_timeout {
        builder = builder.timeout(t);
    }
    if allow_invalid_bundle_tls() {
        builder = builder.danger_accept_invalid_certs(true);
    }
    builder
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
    let client = bundle_http_client(&base, Some(std::time::Duration::from_secs(600)))?;
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
    let response = response.error_for_status().map_err(|e| {
        anyhow!(
            "błąd HTTP manifestu: {}",
            redact_query_strings(&e.to_string())
        )
    })?;
    let body = read_capped_manifest_body(response).await?;
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

    // The `files[].name` fields come from an UNTRUSTED remote manifest and are
    // written to disk BEFORE the registry validates `file_name`, and a hostile
    // server can match any sha256 — so containment must not lean on the hash.
    // Gate every entry against the shared server-side allowlist AND the exact
    // expected set: the model's own `file_name` plus its one allowed ONNX
    // external-data sidecar (`<file_name>.data`, mirroring the `/models/file`
    // resolver). Anything else is refused before a byte is downloaded.
    if !validate_file_name(&file_name) {
        return Err(anyhow!(
            "metadane modelu: nazwa pliku '{}' odrzucona przez allowlistę",
            file_name
        ));
    }
    let sidecar = format!("{}.data", file_name);
    let expected_names: Vec<&str> = if validate_file_name(&sidecar) {
        vec![file_name.as_str(), sidecar.as_str()]
    } else {
        vec![file_name.as_str()]
    };

    let entries = manifest
        .get("files")
        .and_then(|f| f.as_array())
        .ok_or_else(|| anyhow!("manifest bez tablicy 'files'"))?;

    let bearer = api_key.trim();
    let bearer_opt = (!bearer.is_empty()).then_some(bearer);
    let client = bundle_http_client(&base, Some(std::time::Duration::from_secs(600)))?;
    let dir = vision_models_dir();
    std::fs::create_dir_all(&dir).map_err(|e| anyhow!("create {}: {}", dir.display(), e))?;

    let mut written_files: Vec<PathBuf> = Vec::new();
    let cleanup = |files: &[PathBuf]| {
        for f in files {
            let _ = std::fs::remove_file(f);
        }
    };

    let mut cumulative: u64 = 0;
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or_default();
        if name.is_empty() {
            cleanup(&written_files);
            return Err(anyhow!("wpis manifestu bez nazwy pliku"));
        }
        // Fail-closed containment: allowlist first, then the exact expected set.
        if !validate_file_name(name) {
            cleanup(&written_files);
            return Err(anyhow!("nazwa pliku '{}' odrzucona przez allowlistę", name));
        }
        if !expected_names.iter().any(|n| *n == name) {
            cleanup(&written_files);
            return Err(anyhow!(
                "wpis '{}' nie należy do modelu '{}' (dozwolone: {:?})",
                name,
                model_name,
                expected_names
            ));
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
        let declared_size = entry.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
        let rel_url = entry.get("url").and_then(|u| u.as_str()).ok_or_else(|| {
            cleanup(&written_files);
            anyhow!("wpis '{}' bez url", name)
        })?;
        let file_url = resolve_manifest_file_url(&base, rel_url).map_err(|why| {
            cleanup(&written_files);
            anyhow!("wpis '{}': {}", name, why)
        })?;
        let max_bytes = match per_file_ceiling(declared_size, cumulative) {
            Ok(v) => v,
            Err(why) => {
                cleanup(&written_files);
                return Err(anyhow!("wpis '{}': {}", name, why));
            }
        };
        let dest = match contained_model_dest(&dir, name) {
            Ok(d) => d,
            Err(e) => {
                cleanup(&written_files);
                return Err(e);
            }
        };
        if let Some(s) = log_sink {
            s.phase("downloading-vision", &format!("Pobieram {}", name));
        }
        let progress: Option<ProgressFn> = log_sink
            .cloned()
            .map(|sink| progress_for_sink(sink, name.to_string()));
        match download_signed_file(
            &client, file_url, bearer_opt, &dest, name, max_bytes, progress,
        )
        .await
        {
            Ok(written) => cumulative += written,
            Err(e) => {
                cleanup(&written_files);
                return Err(e);
            }
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
        assert!(
            resolve_manifest_file_url(&base(), "http://node-a.example:8090/models/file/x/y")
                .is_err()
        );
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

    #[test]
    fn manifest_entry_name_allowlist_rejects_traversal() {
        // The import loop gates every UNTRUSTED remote manifest entry name
        // through this shared allowlist BEFORE any download, so a `../x`
        // traversal entry never reaches the filesystem.
        assert!(!validate_file_name("../x"));
        assert!(!validate_file_name("../../etc/passwd"));
        assert!(!validate_file_name("sub/dir.onnx"));
        assert!(!validate_file_name(".hidden.onnx"));
        assert!(validate_file_name("model.onnx"));
        assert!(validate_file_name("model.onnx.data"));
    }

    #[test]
    fn contained_model_dest_rejects_traversal() {
        let tmp = std::env::temp_dir().join(format!("tf-cv-contain-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // A plain allowlisted name resolves inside the dir.
        let dest = contained_model_dest(&tmp, "model.onnx").expect("plain name contained");
        assert_eq!(
            dest.file_name().and_then(|s| s.to_str()),
            Some("model.onnx")
        );
        // A traversal name escapes the dir and is rejected.
        assert!(contained_model_dest(&tmp, "../evil.onnx").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn vet_bundle_host_rejects_private_ip_literals() {
        // IP literals resolve without touching real DNS; the default (no global
        // pool → opt-out false) must reject loopback/private targets.
        for host in ["127.0.0.1", "10.1.2.3", "192.168.0.5", "169.254.169.254"] {
            let url =
                reqwest::Url::parse(&format!("https://{host}:8090/models/manifest/x")).unwrap();
            assert!(
                vet_bundle_host(&url).is_err(),
                "private host {host} must be rejected"
            );
        }
    }

    #[test]
    fn vet_bundle_host_accepts_public_ip_literal() {
        let url = reqwest::Url::parse("https://93.184.216.34:443/models/manifest/x").unwrap();
        assert!(vet_bundle_host(&url).is_ok());
    }

    #[test]
    fn per_file_ceiling_enforces_declared_and_total() {
        // Declared size is the ceiling when present.
        assert_eq!(per_file_ceiling(100, 0).unwrap(), 100);
        // A size-less entry falls back to the remaining budget.
        assert_eq!(per_file_ceiling(0, 0).unwrap(), TOTAL_IMPORT_LIMIT);
        // Declared size past the remaining budget is rejected.
        assert!(per_file_ceiling(TOTAL_IMPORT_LIMIT + 1, 0).is_err());
        // A full cumulative budget rejects the next file.
        assert!(per_file_ceiling(1, TOTAL_IMPORT_LIMIT).is_err());
    }
}
