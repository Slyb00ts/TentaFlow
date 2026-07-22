// ===== File: dispatch/ml_studio_remote_import.rs — cross-instance project import (client) =====
//
// Pulls an ML Studio project's export archive from a REMOTE, UNPAIRED instance over
// HTTPS (a "share" URL + API key) and imports it as a NEW local project owned by the
// caller. Core (never the browser) performs the HTTPS pull through the SAME
// no-redirect / DNS-pinned / query-redacting client the vision-model bundle path uses
// (`vision::camera_cv_models::bundle_http_client`), so a hostile serving node or a
// rebinding DNS record cannot steer the Bearer key at an internal service.
//
// Two remote endpoints on the source instance:
//   GET <base>/ml-studio/share/<project_id>/manifest  (Bearer) → JSON preview
//   GET <base>/ml-studio/share/<project_id>/archive   (Bearer) → the ZIP
// The manifest's `archive.url` is an origin-relative `/ml-studio/share/<id>/archive`
// path, re-pinned to the manifest origin before it is fetched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, Result};
use futures::StreamExt;
use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    MessageBody, MlStudioImportDatasetInfo, MlStudioPayload, MlStudioRemoteImportPreviewResponse,
    MlStudioRemoteImportStartResponse, MlStudioRemoteImportStatusResponse, ProtocolError,
    ProtocolErrorCode,
};

use super::HandlerContext;
use crate::ml_studio::project_archive::{self, ImportMode};
use crate::services::rbac::OrgContext;
use crate::vision::camera_cv_models::{
    bundle_http_client, read_capped_manifest_body, redact_query_strings,
};

/// Hard ceiling on the downloaded archive, matching the import-side unpack budget in
/// `project_archive` (`MAX_IMPORT_BYTES`). A lying Content-Length or an unbounded
/// body is rejected before/while writing.
const MAX_REMOTE_ARCHIVE_BYTES: u64 = 32 * 1024 * 1024 * 1024;

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

// ---------------------------------------------------------------------------
// Share-URL parsing
// ---------------------------------------------------------------------------

/// Parses a share URL into the pinned `/ml-studio/share/<id>/manifest` URL plus the
/// project id. Accepts either a direct `.../manifest` URL or any URL whose path
/// contains `/ml-studio/share/<id>` (e.g. a bare share base). Requires https so the
/// Bearer key never travels in clear text. The rebuilt manifest URL drops any query
/// and fragment from the pasted link.
fn parse_share_url(raw: &str) -> std::result::Result<(reqwest::Url, String), String> {
    let parsed =
        reqwest::Url::parse(raw.trim()).map_err(|e| format!("nieprawidłowy URL: {e}"))?;
    if parsed.scheme() != "https" {
        return Err("URL udostępniania musi używać https".to_string());
    }
    let segments: Vec<String> = parsed
        .path_segments()
        .map(|s| s.filter(|x| !x.is_empty()).map(str::to_string).collect())
        .unwrap_or_default();
    let share_pos = segments.iter().position(|s| s == "share").ok_or_else(|| {
        "URL musi wskazywać na /ml-studio/share/<projekt> innej instancji".to_string()
    })?;
    if share_pos == 0 || segments.get(share_pos - 1).map(String::as_str) != Some("ml-studio") {
        return Err("URL musi wskazywać na /ml-studio/share/<projekt>".to_string());
    }
    let project_id = segments
        .get(share_pos + 1)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "URL nie zawiera identyfikatora projektu".to_string())?
        .clone();
    // The id becomes a path segment on the outgoing request; reject any traversal or
    // separator so it cannot rewrite the endpoint path.
    if project_id.contains('/') || project_id.contains('\\') || project_id == ".." {
        return Err("nieprawidłowy identyfikator projektu w URL".to_string());
    }
    let mut manifest_url = parsed.clone();
    manifest_url.set_query(None);
    manifest_url.set_fragment(None);
    manifest_url.set_path(&format!("/ml-studio/share/{project_id}/manifest"));
    Ok((manifest_url, project_id))
}

/// SSRF guard for the manifest's `archive.url`: it MUST be an origin-relative
/// `/ml-studio/share/` path (an absolute or scheme-relative url in a hostile
/// manifest must not steer the pull elsewhere), and the joined result must stay on
/// the manifest URL's exact scheme/host/port. Mirrors
/// `camera_cv_models::resolve_manifest_file_url`.
fn resolve_archive_url(
    base: &reqwest::Url,
    rel_url: &str,
) -> std::result::Result<reqwest::Url, String> {
    if !rel_url.starts_with("/ml-studio/share/") {
        return Err("archive url nie jest ścieżką /ml-studio/share/ — odrzucony".to_string());
    }
    let archive_url = base
        .join(rel_url)
        .map_err(|e| format!("resolve archive url: {e}"))?;
    if archive_url.scheme() != base.scheme()
        || archive_url.host_str() != base.host_str()
        || archive_url.port_or_known_default() != base.port_or_known_default()
    {
        return Err("archive url wychodzi poza origin manifestu — odrzucony".to_string());
    }
    Ok(archive_url)
}

// ---------------------------------------------------------------------------
// Remote manifest fetch
// ---------------------------------------------------------------------------

/// Parsed subset of a remote share manifest.
struct RemoteManifest {
    project_name: String,
    project_type: String,
    datasets: Vec<MlStudioImportDatasetInfo>,
    classes: Vec<String>,
    archive_bytes: u64,
    /// `None` when the manifest omits sha256 (metadata-only manifest that did not
    /// build the archive). Integrity is then verified against the archive
    /// endpoint's `X-Archive-Sha256` header instead.
    archive_sha256: Option<String>,
    archive_rel_url: String,
    archive_version: u32,
}

/// GETs `<base>/ml-studio/share/<id>/manifest` with the API key and parses it.
/// Server-side only: no-redirect + DNS-pinned client, body-size cap, query-string
/// redaction on errors. The key travels ONLY to the manifest origin.
async fn fetch_remote_manifest(
    manifest_url: &reqwest::Url,
    api_key: &str,
) -> Result<RemoteManifest> {
    let bearer = api_key.trim();
    let client = bundle_http_client(manifest_url)?;
    let mut request = client.get(manifest_url.clone());
    if !bearer.is_empty() {
        request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {bearer}"));
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
    let manifest: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| anyhow!("parsowanie JSON manifestu: {e}"))?;

    let project = manifest
        .get("project")
        .ok_or_else(|| anyhow!("manifest bez obiektu 'project'"))?;
    let project_name = project
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let project_type = project
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let datasets = manifest
        .get("datasets")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|d| {
                    Some(MlStudioImportDatasetInfo {
                        dataset_id: d.get("id")?.as_str()?.to_string(),
                        name: d
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        image_count: d.get("image_count").and_then(|v| v.as_u64()).unwrap_or(0),
                        annotation_count: d
                            .get("annotation_count")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let classes = manifest
        .get("classes")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let archive = manifest
        .get("archive")
        .ok_or_else(|| anyhow!("manifest bez obiektu 'archive'"))?;
    let archive_bytes = archive.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
    // OPTIONAL: a metadata-only manifest (the source did not build the archive to
    // stay under the request timeout) omits sha256. A present value must still be a
    // well-formed digest; integrity is otherwise confirmed against the archive
    // endpoint's `X-Archive-Sha256` header at download time.
    let archive_sha256 = archive
        .get("sha256")
        .and_then(|v| v.as_str())
        .map(str::to_ascii_lowercase)
        .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()));
    let archive_rel_url = archive
        .get("url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("manifest bez url archiwum"))?
        .to_string();
    let archive_version = manifest
        .get("archive_version")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    Ok(RemoteManifest {
        project_name,
        project_type,
        datasets,
        classes,
        archive_bytes,
        archive_sha256,
        archive_rel_url,
        archive_version,
    })
}

// ---------------------------------------------------------------------------
// Archive download
// ---------------------------------------------------------------------------

/// Streams the remote archive to `dest`, enforcing a Content-Length ceiling and a
/// mid-stream byte ceiling, then verifies its sha256. Mirrors
/// `camera_cv_models::download_signed_file`: no-redirect (a 3xx is rejected since
/// `error_for_status` treats it as success), token-bearing query redacted, atomic
/// `<dest>.partial` → rename. Returns the number of bytes written. Deletes the
/// partial/dest on any failure.
///
/// Integrity precedence, fail-closed: the expected digest is the MANIFEST sha256 if
/// present, else the archive endpoint's `X-Archive-Sha256` response header. If a
/// manifest sha256 AND the header are both present they must agree. If NEITHER is
/// available the download is rejected — unverified bytes are never imported.
async fn download_archive(
    client: &reqwest::Client,
    url: reqwest::Url,
    bearer: &str,
    dest: &Path,
    manifest_sha256: Option<&str>,
    progress_job: &str,
) -> Result<u64> {
    use std::io::Write;

    let mut request = client.get(url);
    if !bearer.is_empty() {
        request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    let response = request
        .send()
        .await
        .map_err(|e| anyhow!("GET archiwum: {}", redact_query_strings(&e.to_string())))?;
    if response.status().is_redirection() {
        return Err(anyhow!(
            "pobieranie archiwum: przekierowanie ({}) — nie jest śledzone",
            response.status()
        ));
    }
    let response = response.error_for_status().map_err(|e| {
        anyhow!(
            "pobieranie archiwum, błąd HTTP: {}",
            redact_query_strings(&e.to_string())
        )
    })?;

    // Whole-archive digest advertised by the source (present even when the manifest
    // omitted it). Normalized + validated like the manifest value.
    let header_sha256 = response
        .headers()
        .get("X-Archive-Sha256")
        .and_then(|v| v.to_str().ok())
        .map(str::to_ascii_lowercase)
        .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()));

    // Resolve the expected digest BEFORE spending bandwidth: manifest wins, header
    // is the fallback, disagreement is fatal, and absence of both fails closed.
    let expected_sha256 = match (manifest_sha256, header_sha256.as_deref()) {
        (Some(m), Some(h)) if m != h => {
            return Err(anyhow!(
                "niezgodna suma sha256 archiwum: manifest ({m}) vs nagłówek ({h}) — odrzucono"
            ))
        }
        (Some(m), _) => m.to_string(),
        (None, Some(h)) => h.to_string(),
        (None, None) => {
            return Err(anyhow!(
                "brak sumy sha256 archiwum (ani w manifeście, ani w nagłówku X-Archive-Sha256) — nie importuję niezweryfikowanych danych"
            ))
        }
    };

    if response.content_length().unwrap_or(0) > MAX_REMOTE_ARCHIVE_BYTES {
        return Err(anyhow!(
            "archiwum przekracza limit {MAX_REMOTE_ARCHIVE_BYTES} B (Content-Length)"
        ));
    }
    let total = response.content_length().unwrap_or(0);
    set_progress(progress_job, |p| {
        if total > 0 {
            p.bytes_total = total;
        }
    });

    let partial = dest.with_extension("zip.partial");
    let mut file = std::fs::File::create(&partial)
        .map_err(|e| anyhow!("utworzenie {}: {e}", partial.display()))?;

    let mut downloaded: u64 = 0;
    let mut last_tick: u64 = 0;
    const PROGRESS_INTERVAL_BYTES: u64 = 4 * 1024 * 1024;

    let mut stream = response.bytes_stream();
    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res
            .map_err(|e| anyhow!("strumień archiwum: {}", redact_query_strings(&e.to_string())))?;
        downloaded += chunk.len() as u64;
        if downloaded > MAX_REMOTE_ARCHIVE_BYTES {
            drop(file);
            let _ = std::fs::remove_file(&partial);
            return Err(anyhow!(
                "archiwum przekroczyło limit {MAX_REMOTE_ARCHIVE_BYTES} B w trakcie pobierania — usunięto"
            ));
        }
        if let Err(e) = file.write_all(&chunk) {
            drop(file);
            let _ = std::fs::remove_file(&partial);
            return Err(anyhow!("zapis {}: {e}", partial.display()));
        }
        if downloaded - last_tick >= PROGRESS_INTERVAL_BYTES {
            set_progress(progress_job, |p| p.bytes_done = downloaded);
            last_tick = downloaded;
        }
    }
    file.flush()
        .map_err(|e| anyhow!("flush {}: {e}", partial.display()))?;
    drop(file);

    std::fs::rename(&partial, dest)
        .map_err(|e| anyhow!("publikacja {}: {e}", dest.display()))?;
    set_progress(progress_job, |p| p.bytes_done = downloaded);

    let actual = crate::api::model_bundle::sha256_file_hex(dest)
        .await
        .map_err(|e| anyhow!("suma sha256 archiwum: {e}"))?;
    if actual != expected_sha256 {
        let _ = std::fs::remove_file(dest);
        return Err(anyhow!(
            "niezgodna suma sha256 archiwum (oczekiwano {expected_sha256}, otrzymano {actual}) — usunięto"
        ));
    }
    Ok(downloaded)
}

// ---------------------------------------------------------------------------
// Background job progress
// ---------------------------------------------------------------------------

/// Live progress of a remote-import job, polled by the UI. `status` is
/// "running" | "succeeded" | "failed"; `phase` is "downloading" then the inner
/// import phases ("extracting" | "registering"). `owner_user_id` authorizes the
/// status caller — a bare job id must not expose progress to an unrelated user.
#[derive(Clone, Debug, Default)]
struct RemoteImportProgress {
    status: String,
    phase: String,
    bytes_total: u64,
    bytes_done: u64,
    owner_user_id: String,
    error: Option<String>,
}

static PROGRESS: OnceLock<Mutex<HashMap<String, RemoteImportProgress>>> = OnceLock::new();

fn progress_map() -> &'static Mutex<HashMap<String, RemoteImportProgress>> {
    PROGRESS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn init_progress(job_id: &str, p: RemoteImportProgress) {
    if let Ok(mut m) = progress_map().lock() {
        m.insert(job_id.to_string(), p);
    }
}

fn set_progress(job_id: &str, f: impl FnOnce(&mut RemoteImportProgress)) {
    if let Ok(mut m) = progress_map().lock() {
        if let Some(p) = m.get_mut(job_id) {
            f(p);
        }
    }
}

fn job_progress(job_id: &str) -> Option<RemoteImportProgress> {
    progress_map().lock().ok()?.get(job_id).cloned()
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[handler(variant = "MlStudioRemoteImportPreviewRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_remote_import_preview(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RemoteImportPreviewRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioRemoteImportPreviewRequest",
            ))
        }
    };

    let respond = |resp: MlStudioRemoteImportPreviewResponse| {
        Ok(MessageBody::MlStudioBody(
            MlStudioPayload::RemoteImportPreviewResponse(resp),
        ))
    };

    let (manifest_url, _project_id) = match parse_share_url(&payload.url) {
        Ok(v) => v,
        Err(e) => {
            return respond(MlStudioRemoteImportPreviewResponse {
                project_name: String::new(),
                project_type: String::new(),
                datasets: Vec::new(),
                classes: Vec::new(),
                archive_bytes: 0,
                archive_version: 0,
                error: Some(e),
            })
        }
    };

    match fetch_remote_manifest(&manifest_url, &payload.api_key).await {
        Ok(m) => respond(MlStudioRemoteImportPreviewResponse {
            project_name: m.project_name,
            project_type: m.project_type,
            datasets: m.datasets,
            classes: m.classes,
            archive_bytes: m.archive_bytes,
            archive_version: m.archive_version,
            error: None,
        }),
        Err(e) => respond(MlStudioRemoteImportPreviewResponse {
            project_name: String::new(),
            project_type: String::new(),
            datasets: Vec::new(),
            classes: Vec::new(),
            archive_bytes: 0,
            archive_version: 0,
            error: Some(format!("{e:#}")),
        }),
    }
}

#[handler(variant = "MlStudioRemoteImportStartRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_remote_import_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RemoteImportStartRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioRemoteImportStartRequest",
            ))
        }
    };
    let org = require_org(ctx)?;

    let (manifest_url, _project_id) =
        parse_share_url(&payload.url).map_err(|e| ProtocolError::bad_request(e))?;

    // Job id is returned NOW and every slow step (manifest fetch, the remote's own
    // first-download archive build, the download, the import) runs inside the polled
    // background job — nothing network-bound happens in this request-response path,
    // so the handler always returns in well under a second regardless of remote speed.
    let job_id = uuid::Uuid::new_v4().to_string();
    init_progress(
        &job_id,
        RemoteImportProgress {
            status: "running".to_string(),
            phase: "connecting".to_string(),
            owner_user_id: org.user_id.clone(),
            ..RemoteImportProgress::default()
        },
    );

    let api_key = payload.api_key.clone();
    let name_override = payload
        .name_override
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let owner_user_id = org.user_id.clone();
    let org_id = org.org_id.clone();
    let job_task = job_id.clone();
    tokio::spawn(async move {
        if let Err(e) =
            run_remote_import(&job_task, manifest_url, api_key, name_override, owner_user_id, org_id)
                .await
        {
            tracing::warn!(job_id = %job_task, error = %e, "ml studio remote import failed");
            set_progress(&job_task, |p| {
                p.status = "failed".to_string();
                p.error = Some(format!("{e:#}"));
            });
        }
    });

    Ok(MessageBody::MlStudioBody(
        MlStudioPayload::RemoteImportStartResponse(MlStudioRemoteImportStartResponse { job_id }),
    ))
}

/// Downloads the archive (phase DOWNLOAD) and then imports it as a new project
/// (phase IMPORT), surfacing the inner `project_archive` job's progress. The staged
/// zip is always deleted on completion or failure.
async fn run_remote_import(
    job_id: &str,
    manifest_url: reqwest::Url,
    api_key: String,
    name_override: Option<String>,
    owner_user_id: String,
    org_id: String,
) -> Result<()> {
    let bearer = api_key.trim().to_string();
    let manifest = fetch_remote_manifest(&manifest_url, &bearer).await?;
    let archive_url = resolve_archive_url(&manifest_url, &manifest.archive_rel_url)
        .map_err(|why| anyhow!("archiwum: {why}"))?;

    set_progress(job_id, |p| {
        p.phase = "downloading".to_string();
        if manifest.archive_bytes > 0 {
            p.bytes_total = manifest.archive_bytes;
        }
    });

    let staging_dir = crate::paths::ml_studio_import_staging_dir();
    std::fs::create_dir_all(&staging_dir)
        .map_err(|e| anyhow!("utworzenie katalogu importu {}: {e}", staging_dir.display()))?;
    let staged: PathBuf = staging_dir.join(format!("mlsimp_{}.zip", uuid::Uuid::new_v4().simple()));

    let client = bundle_http_client(&manifest_url)?;
    let download = download_archive(
        &client,
        archive_url,
        &bearer,
        &staged,
        manifest.archive_sha256.as_deref(),
        job_id,
    )
    .await;
    if let Err(e) = download {
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }

    // Phase 2: hand the verified zip to the shared import path as a NEW project.
    set_progress(job_id, |p| p.phase = "extracting".to_string());
    let inner = project_archive::spawn_import(
        staged.clone(),
        ImportMode::NewProject { name_override },
        owner_user_id,
        org_id,
    );
    let inner_job = match inner {
        Ok(id) => id,
        Err(e) => {
            let _ = std::fs::remove_file(&staged);
            return Err(anyhow!("start importu: {e}"));
        }
    };

    // Mirror the inner import job's progress until it reaches a terminal state.
    let result = loop {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let Some(inner_p) = project_archive::job_progress(&inner_job) else {
            break Err(anyhow!("zadanie importu zniknęło"));
        };
        set_progress(job_id, |p| {
            p.phase = inner_p.phase.clone();
            if inner_p.bytes_total > 0 {
                p.bytes_total = inner_p.bytes_total;
                p.bytes_done = inner_p.bytes_done;
            }
        });
        match inner_p.status.as_str() {
            "succeeded" => break Ok(()),
            "failed" => {
                break Err(anyhow!(inner_p
                    .error
                    .unwrap_or_else(|| "import archiwum nie powiódł się".to_string())))
            }
            _ => {}
        }
    };

    let _ = std::fs::remove_file(&staged);
    match result {
        Ok(()) => {
            set_progress(job_id, |p| {
                p.status = "succeeded".to_string();
                p.phase = "registering".to_string();
            });
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[handler(variant = "MlStudioRemoteImportStatusRequest", since = (1, 0))]
#[policy(PowerUser)]
#[observed]
pub async fn ml_studio_remote_import_status(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::MlStudioBody(MlStudioPayload::RemoteImportStatusRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected MlStudioRemoteImportStatusRequest",
            ))
        }
    };
    let org = require_org(ctx)?;

    let progress = job_progress(&payload.job_id)
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "unknown job"))?;
    // A job id alone must not expose another user's import progress.
    if progress.owner_user_id != org.user_id {
        return Err(ProtocolError::new(ProtocolErrorCode::NotFound, "unknown job"));
    }

    Ok(MessageBody::MlStudioBody(
        MlStudioPayload::RemoteImportStatusResponse(MlStudioRemoteImportStatusResponse {
            status: progress.status,
            phase: progress.phase,
            bytes_total: progress.bytes_total,
            bytes_done: progress.bytes_done,
            error: progress.error,
        }),
    ))
}
