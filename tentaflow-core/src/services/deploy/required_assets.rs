// =============================================================================
// File: services/deploy/required_assets.rs
// Description: Materialises `[[required_asset]]` engine files into the central
//              host directory `paths::engine_assets_dir(<engine_id>)` before a
//              deploy starts. ONE copy serves every variant: docker bind-mounts
//              it read-only at `mount_path`, native passes the host path via
//              `env_var`. The embedded bundle deliberately excludes such files
//              (`*.onnx`, `*.gguf`, `*.pth`, `*.safetensors`), so they are
//              copied from the source checkout or downloaded and checksummed.
// =============================================================================

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{DeployError, DeployResult, LogSink};
use crate::services::manifest::{RequiredAsset, ServiceManifest};
use crate::services::model_download::download_with_progress;

/// Where an asset came from. Callers log it; tests assert on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetSource {
    /// The central directory already held a valid copy.
    AlreadyPresent,
    /// Copied from the source checkout the binary was built from.
    Repo,
    /// Fetched from the manifest URL and verified.
    Downloaded,
}

/// Sha256 of a file as lowercase hex, streamed so a multi-GB asset never
/// lands in memory.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn matches_sha(path: &Path, expected: &str) -> bool {
    sha256_file(path)
        .map(|actual| actual.eq_ignore_ascii_case(expected))
        .unwrap_or(false)
}

/// Host path of one asset. `None` when `path` is not a plain file name
/// (defence in depth — build.rs already rejects such a manifest).
pub fn host_path(assets_dir: &Path, asset: &RequiredAsset) -> Option<PathBuf> {
    let name = Path::new(&asset.path);
    let mut components = name.components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => Some(assets_dir.join(name)),
        _ => None,
    }
}

fn copy_file(from: &Path, to: &Path) -> DeployResult<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            DeployError::Other(format!(
                "required asset: create {}: {}",
                parent.display(),
                e
            ))
        })?;
    }
    std::fs::copy(from, to).map_err(|e| {
        DeployError::Other(format!(
            "required asset: copy {} -> {}: {}",
            from.display(),
            to.display(),
            e
        ))
    })?;
    Ok(())
}

/// Everything doable without the network: keep a valid central copy, or
/// restore it from the source checkout. `Ok(None)` means the asset still has
/// to be downloaded.
pub fn materialize_offline(
    asset: &RequiredAsset,
    assets_dir: &Path,
    repo_root: Option<&Path>,
) -> DeployResult<Option<AssetSource>> {
    let dest = host_path(assets_dir, asset).ok_or_else(|| {
        DeployError::Manifest(format!(
            "required_asset.path '{}' is not a plain file name",
            asset.path
        ))
    })?;

    if dest.is_file() {
        if matches_sha(&dest, &asset.sha256) {
            return Ok(Some(AssetSource::AlreadyPresent));
        }
        // A truncated or replaced copy would start the engine with a wrong
        // model — drop it and fetch again.
        let _ = std::fs::remove_file(&dest);
    }

    if let (Some(root), Some(rel)) = (repo_root, asset.repo_path.as_deref()) {
        let source = root.join(rel);
        if source.is_file() && matches_sha(&source, &asset.sha256) {
            copy_file(&source, &dest)?;
            return Ok(Some(AssetSource::Repo));
        }
    }

    Ok(None)
}

/// Downloads one asset into the central directory and verifies its checksum.
async fn download_asset(
    asset: &RequiredAsset,
    assets_dir: &Path,
    log: Option<&LogSink>,
) -> DeployResult<()> {
    let dest = host_path(assets_dir, asset).ok_or_else(|| {
        DeployError::Manifest(format!(
            "required_asset.path '{}' is not a plain file name",
            asset.path
        ))
    })?;
    let label = asset.path.clone();

    std::fs::create_dir_all(assets_dir).map_err(|e| {
        DeployError::Other(format!(
            "required asset: create {}: {}",
            assets_dir.display(),
            e
        ))
    })?;

    // Download to a staging name; the final path appears only after the
    // checksum matched, so an existing central copy is always trustworthy.
    let staging = assets_dir.join(format!("{}.incoming", asset.path));
    let _ = std::fs::remove_file(&staging);

    let progress = log.map(|sink| {
        let sink = sink.clone();
        let label = label.clone();
        Box::new(move |done: u64, total: u64, _l: &str| {
            let pct = if total > 0 {
                ((done as f64 / total as f64) * 100.0).min(100.0) as u8
            } else {
                0
            };
            sink.progress(
                "fetch-assets",
                pct,
                &format!("[assets] {} {} / {} KB", label, done / 1024, total / 1024),
            );
        }) as crate::services::model_download::ProgressFn
    });

    if let Some(sink) = log {
        sink.phase(
            "fetch-assets",
            &format!("[assets] downloading {} from {}", label, asset.url),
        );
    }

    download_with_progress(&asset.url, &staging, &label, progress)
        .await
        .map_err(|e| {
            let _ = std::fs::remove_file(&staging);
            DeployError::Other(format!(
                "required asset {}: download from {} failed: {:#}",
                label, asset.url, e
            ))
        })?;

    finalize_download(asset, &staging, &dest)
}

/// Verifies a freshly downloaded file and publishes it under its final name.
/// A mismatch deletes the download and fails the deploy — a wrong artifact
/// must never reach a running engine.
fn finalize_download(asset: &RequiredAsset, staging: &Path, dest: &Path) -> DeployResult<()> {
    if !matches_sha(staging, &asset.sha256) {
        let actual = sha256_file(staging).unwrap_or_else(|_| "<unreadable>".to_string());
        let _ = std::fs::remove_file(staging);
        return Err(DeployError::Other(format!(
            "required asset {}: sha256 mismatch (expected {}, got {}) from {}",
            asset.path, asset.sha256, actual, asset.url
        )));
    }
    std::fs::rename(staging, dest).map_err(|e| {
        DeployError::Other(format!(
            "required asset {}: publish {}: {}",
            asset.path,
            dest.display(),
            e
        ))
    })
}

/// Makes every asset of `manifest` present in the central engine-assets
/// directory. Runs for EVERY deploy variant before the strategy prepares, so
/// docker and native share one file. Order per asset: valid central copy →
/// source checkout → download + verify.
pub async fn ensure_required_assets(
    manifest: &ServiceManifest,
    log: Option<&LogSink>,
) -> DeployResult<()> {
    if manifest.required_assets.is_empty() {
        return Ok(());
    }
    let assets_dir = crate::paths::engine_assets_dir(&manifest.engine.id);
    let repo_root = crate::paths::repo_root();

    for asset in &manifest.required_assets {
        let source = match materialize_offline(asset, &assets_dir, repo_root.as_deref())? {
            Some(source) => source,
            None => {
                download_asset(asset, &assets_dir, log).await?;
                AssetSource::Downloaded
            }
        };
        if let Some(sink) = log {
            sink.phase(
                "fetch-assets",
                &format!(
                    "[assets] {} ready in {} ({:?})",
                    asset.path,
                    assets_dir.display(),
                    source
                ),
            );
        }
        tracing::info!(
            engine = %manifest.engine.id,
            asset = %asset.path,
            ?source,
            "required asset ready"
        );
    }
    Ok(())
}

/// Read-only bind mounts (host_path, container_path, read_only) for the
/// engine's assets. The caller has already run `ensure_required_assets`, so a
/// missing file here means the deploy path skipped that step.
pub fn container_binds(manifest: &ServiceManifest) -> DeployResult<Vec<(PathBuf, String, bool)>> {
    let assets_dir = crate::paths::engine_assets_dir(&manifest.engine.id);
    let mut binds = Vec::with_capacity(manifest.required_assets.len());
    for asset in &manifest.required_assets {
        let host = host_path(&assets_dir, asset).ok_or_else(|| {
            DeployError::Manifest(format!(
                "required_asset.path '{}' is not a plain file name",
                asset.path
            ))
        })?;
        if !host.is_file() {
            return Err(DeployError::Other(format!(
                "required asset {} missing at {} — engine '{}' cannot start without it",
                asset.path,
                host.display(),
                manifest.engine.id
            )));
        }
        binds.push((host, asset.mount_path.clone(), true));
    }
    Ok(binds)
}

/// Fails when any declared asset is missing from the central directory —
/// used by the native variant, which mounts nothing but must not start an
/// engine whose model file is absent.
pub fn verify_present(manifest: &ServiceManifest) -> DeployResult<()> {
    container_binds(manifest).map(|_| ())
}

/// `NAME=value` entries for the assets that declare an `env_var`. `in_container`
/// picks `mount_path`; otherwise the process gets the host path.
pub fn asset_env(manifest: &ServiceManifest, in_container: bool) -> Vec<(String, String)> {
    let assets_dir = crate::paths::engine_assets_dir(&manifest.engine.id);
    manifest
        .required_assets
        .iter()
        .filter_map(|asset| {
            let name = asset.env_var.clone()?;
            let value = if in_container {
                asset.mount_path.clone()
            } else {
                host_path(&assets_dir, asset)?
                    .to_string_lossy()
                    .into_owned()
            };
            Some((name, value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTENT: &[u8] = b"silero-vad-stand-in";

    fn content_sha() -> String {
        let mut h = Sha256::new();
        h.update(CONTENT);
        hex::encode(h.finalize())
    }

    fn asset(sha: &str) -> RequiredAsset {
        RequiredAsset {
            path: "silero_vad.onnx".to_string(),
            mount_path: "/opt/models/silero_vad.onnx".to_string(),
            url: "https://example.invalid/silero_vad.onnx".to_string(),
            sha256: sha.to_string(),
            repo_path: Some(
                "tentaflow-containers/agents/native/teams-bot/models/silero_vad.onnx".to_string(),
            ),
            env_var: Some("VAD_MODEL_PATH".to_string()),
        }
    }

    #[test]
    fn sha256_file_matches_known_digest() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("blob.bin");
        std::fs::write(&file, CONTENT).unwrap();
        assert_eq!(sha256_file(&file).unwrap(), content_sha());
        assert!(matches_sha(&file, &content_sha()));
        assert!(!matches_sha(&file, &"0".repeat(64)));
    }

    #[test]
    fn valid_central_copy_is_kept_without_fetching() {
        let dir = tempfile::tempdir().unwrap();
        let a = asset(&content_sha());
        std::fs::write(dir.path().join(&a.path), CONTENT).unwrap();

        let source = materialize_offline(&a, dir.path(), None).unwrap();
        assert_eq!(source, Some(AssetSource::AlreadyPresent));
        assert_eq!(std::fs::read(dir.path().join(&a.path)).unwrap(), CONTENT);
    }

    #[test]
    fn corrupt_central_copy_is_dropped_and_refetched() {
        let dir = tempfile::tempdir().unwrap();
        let a = asset(&content_sha());
        std::fs::write(dir.path().join(&a.path), b"truncated").unwrap();

        let source = materialize_offline(&a, dir.path(), None).unwrap();
        assert_eq!(source, None);
        assert!(!dir.path().join(&a.path).exists());
    }

    #[test]
    fn repo_copy_wins_over_download() {
        let dir = tempfile::tempdir().unwrap();
        let assets = dir.path().join("assets");
        let repo = dir.path().join("repo");
        let a = asset(&content_sha());
        let src = repo.join(a.repo_path.as_deref().unwrap());
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, CONTENT).unwrap();

        let source = materialize_offline(&a, &assets, Some(&repo)).unwrap();
        assert_eq!(source, Some(AssetSource::Repo));
        assert_eq!(std::fs::read(assets.join(&a.path)).unwrap(), CONTENT);
    }

    #[test]
    fn repo_copy_with_wrong_checksum_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let assets = dir.path().join("assets");
        let repo = dir.path().join("repo");
        let a = asset(&content_sha());
        let src = repo.join(a.repo_path.as_deref().unwrap());
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"stale-revision").unwrap();

        let source = materialize_offline(&a, &assets, Some(&repo)).unwrap();
        assert_eq!(source, None);
        assert!(!assets.join(&a.path).exists());
    }

    #[test]
    fn verified_download_is_published_under_final_name() {
        let dir = tempfile::tempdir().unwrap();
        let a = asset(&content_sha());
        let staging = dir.path().join("silero_vad.onnx.incoming");
        let dest = dir.path().join(&a.path);
        std::fs::write(&staging, CONTENT).unwrap();

        finalize_download(&a, &staging, &dest).unwrap();
        assert!(!staging.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), CONTENT);
    }

    #[test]
    fn download_with_wrong_checksum_fails_and_deletes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = asset(&content_sha());
        let staging = dir.path().join("silero_vad.onnx.incoming");
        let dest = dir.path().join(&a.path);
        std::fs::write(&staging, b"corrupted-transfer").unwrap();

        let err = finalize_download(&a, &staging, &dest).unwrap_err();
        assert!(format!("{}", err).contains("sha256 mismatch"));
        assert!(!staging.exists());
        assert!(!dest.exists());
    }

    #[test]
    fn path_with_separator_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = asset(&content_sha());
        a.path = "../outside.onnx".to_string();
        let err = materialize_offline(&a, dir.path(), None).unwrap_err();
        assert!(matches!(err, DeployError::Manifest(_)));
    }

    #[test]
    fn missing_asset_makes_container_binds_fail_loudly() {
        let manifest = crate::services::manifest::registry()
            .by_id("teams-bot")
            .expect("teams-bot manifest")
            .clone();
        // The central directory is empty in a test environment, so the bind
        // builder must refuse instead of mounting a non-existent path.
        if !crate::paths::engine_assets_dir("teams-bot")
            .join("silero_vad.onnx")
            .is_file()
        {
            let err = container_binds(&manifest).unwrap_err();
            assert!(format!("{}", err).contains("missing at"));
        }
    }

    #[test]
    fn teams_bot_manifest_declares_its_vad_asset() {
        let manifest = crate::services::manifest::registry()
            .by_id("teams-bot")
            .expect("teams-bot manifest");
        let asset = manifest
            .required_assets
            .iter()
            .find(|a| a.path == "silero_vad.onnx")
            .expect("silero_vad required asset survives build.rs re-serialisation");
        assert_eq!(asset.sha256.len(), 64);
        assert!(asset.url.starts_with("https://"));
        assert_eq!(asset.mount_path, "/opt/models/silero_vad.onnx");
        assert_eq!(asset.env_var.as_deref(), Some("VAD_MODEL_PATH"));
        assert_eq!(
            asset_env(manifest, true),
            vec![(
                "VAD_MODEL_PATH".to_string(),
                "/opt/models/silero_vad.onnx".to_string()
            )]
        );
    }
}
