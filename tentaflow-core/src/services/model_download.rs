// =============================================================================
// File: services/model_download.rs
// Description: Streaming HTTP download with progress reporting. Used by deploy
//              strategies (vision) and startup bootstrap (audio) to fetch ONNX
//              models from their upstream sources (HuggingFace, GitHub releases)
//              into the shared `paths::models_root()` cache. Idempotent —
//              skips download when destination already exists with non-zero size.
// =============================================================================

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::Client;

/// Progress callback receives (downloaded_bytes, total_bytes_or_zero, label).
/// `total_bytes` may be 0 if Content-Length header is missing.
pub type ProgressFn = Box<dyn Fn(u64, u64, &str) + Send + Sync>;

/// Streaming download with progress callback. Idempotent — returns Ok(false)
/// when destination exists with non-zero size; Ok(true) when download succeeded.
/// Writes to `<dest>.partial` and renames atomically on success.
pub async fn download_with_progress(
    url: &str,
    dest: &Path,
    label: &str,
    progress: Option<ProgressFn>,
) -> Result<bool> {
    if let Ok(meta) = std::fs::metadata(dest) {
        if meta.len() > 0 {
            tracing::debug!("download skip {} ({}): exists", label, dest.display());
            return Ok(false);
        }
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).context("create parent dir")?;
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(600))
        .connect_timeout(Duration::from_secs(30))
        .user_agent(concat!("tentaflow/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build reqwest client")?;

    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {}", url))?
        .error_for_status()
        .with_context(|| format!("HTTP error from {}", url))?;

    let total = response.content_length().unwrap_or(0);
    let partial = dest.with_extension(format!(
        "{}.partial",
        dest.extension().and_then(|s| s.to_str()).unwrap_or("tmp")
    ));

    let mut file = std::fs::File::create(&partial)
        .with_context(|| format!("create {}", partial.display()))?;

    let mut downloaded: u64 = 0;
    let mut last_progress_bytes: u64 = 0;
    const PROGRESS_INTERVAL_BYTES: u64 = 256 * 1024; // emit every 256 KB

    let mut stream = response.bytes_stream();
    use futures::StreamExt;
    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res.context("stream chunk")?;
        file.write_all(&chunk).context("write chunk")?;
        downloaded += chunk.len() as u64;
        if downloaded - last_progress_bytes >= PROGRESS_INTERVAL_BYTES {
            if let Some(ref cb) = progress {
                cb(downloaded, total, label);
            }
            last_progress_bytes = downloaded;
        }
    }
    file.flush().context("flush")?;
    drop(file);

    std::fs::rename(&partial, dest).with_context(|| {
        format!(
            "rename {} -> {}",
            partial.display(),
            dest.display()
        )
    })?;

    if let Some(ref cb) = progress {
        cb(downloaded, downloaded, label);
    }
    tracing::info!(
        "downloaded {} ({} KB) -> {}",
        label,
        downloaded / 1024,
        dest.display()
    );

    Ok(true)
}

/// Convenience wrapper: ensure file exists at `dest` by downloading from `url`
/// if missing. Returns final path (always == dest). Wraps `download_with_progress`.
pub async fn ensure_model_file(
    url: &str,
    dest: &Path,
    label: &str,
    progress: Option<ProgressFn>,
) -> Result<()> {
    download_with_progress(url, dest, label, progress).await?;
    Ok(())
}
