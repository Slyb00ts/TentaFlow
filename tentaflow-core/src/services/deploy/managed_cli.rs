// ============ File: managed_cli.rs — Versioned installations shared by isolated agent accounts. ============

use std::path::{Path, PathBuf};
use tokio::process::Command;

use super::{DeployError, DeployResult, LogSink};

struct Artifact {
    url: &'static str,
    sha256: &'static str,
    size: u64,
}

fn artifact(engine: &str, version: &str, os: &str, arch: &str) -> DeployResult<Artifact> {
    let (url, sha256, size) = match (engine, version, os, arch) {
        ("muse-code", "1.0.3-R2198.1", "macos", "aarch64") => (
            "https://lookaside.facebook.com/lookaside/muse/download/?channel=muse&version=1.0.3-R2198.1&file=muse-aarch64-macos",
            "4c0f960028b603174af7df7bd5051d8c35d6c1aa372a37d18bc770926a0577a7", 241984144),
        ("muse-code", "1.0.3-R2198.1", "macos", "x86_64") => (
            "https://lookaside.facebook.com/lookaside/muse/download/?channel=muse&version=1.0.3-R2198.1&file=muse-x86-macos",
            "dbcee07bd234fc19805d5d6a358c591b79cc7d781311d10219f856d934843ac2", 263883536),
        ("grok-build", "1.0.13", "macos", "aarch64") => (
            "https://x.ai/cli/grok-1.0.13-macos-aarch64",
            "8669e0fdadceec25b8c159c355f427ffbd82583525d774b6ab1522197ea83b80", 133486016),
        ("grok-build", "1.0.13", "macos", "x86_64") => (
            "https://x.ai/cli/grok-1.0.13-macos-x86_64",
            "8eacec87f5ecdb9259c6d812d12ce9e2d405b1526e36ae9d7fc81ec31dbd74d6", 149694528),
        _ => return Err(DeployError::Manifest(format!("no verified {engine} {version} artifact for {os}/{arch}"))),
    };
    Ok(Artifact { url, sha256, size })
}

fn verify_artifact(path: &Path, expected: &Artifact) -> DeployResult<()> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| DeployError::Other(format!("agent artifact metadata: {e}")))?;
    if !metadata.is_file() || metadata.len() != expected.size {
        return Err(DeployError::Other(
            "agent artifact has an unexpected type or size".into(),
        ));
    }
    let digest = super::required_assets::sha256_file(path)
        .map_err(|e| DeployError::Other(format!("agent artifact checksum: {e}")))?;
    if digest != expected.sha256 {
        return Err(DeployError::Other(
            "agent artifact checksum mismatch".into(),
        ));
    }
    Ok(())
}

async fn acquire_install_lock(lock: &std::fs::File, wait: std::time::Duration) -> DeployResult<()> {
    // Account startups share immutable installations, so contention is expected.
    // Polling keeps cancellation from leaving a blocked thread acquiring the lock later.
    tokio::time::timeout(wait, async {
        loop {
            match lock.try_lock() {
                Ok(()) => return Ok(()),
                Err(std::fs::TryLockError::WouldBlock) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(std::fs::TryLockError::Error(error)) => {
                    return Err(DeployError::Other(format!(
                        "cannot lock agent installation: {error}"
                    )));
                }
            }
        }
    })
    .await
    .map_err(|_| {
        DeployError::Other("timed out waiting for another agent installation to finish".into())
    })?
}

pub(super) async fn install(
    engine: &str,
    version: &str,
    log: Option<&LogSink>,
) -> DeployResult<(PathBuf, PathBuf)> {
    let (package, executable) = match engine {
        "codex" => (Some("@openai/codex"), "codex"),
        "claude-code" => (Some("@anthropic-ai/claude-code"), "claude"),
        "grok-build" => (None, "grok"),
        "muse-code" => (None, "muse"),
        _ => {
            return Err(DeployError::Manifest(format!(
                "managed-cli engine '{engine}' has no installer mapping"
            )))
        }
    };
    if version.is_empty()
        || !version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(DeployError::Manifest("invalid managed CLI version".into()));
    }
    let root = crate::paths::cache_dir()
        .join("coding-agents")
        .join(engine)
        .join(version);
    std::fs::create_dir_all(&root).map_err(|e| DeployError::Other(e.to_string()))?;
    // A second daemon can share this cache; an in-process mutex cannot protect it.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("install.lock"))
        .map_err(|e| DeployError::Other(e.to_string()))?;
    acquire_install_lock(&lock, std::time::Duration::from_secs(300)).await?;
    let bin = if let Some(package) = package {
        let bin = root.join("node_modules/.bin");
        let name = if cfg!(windows) {
            format!("{executable}.cmd")
        } else {
            executable.into()
        };
        let completion = root.join("installation-complete");
        let identity = format!("{package}@{version}");
        if !bin.join(&name).is_file()
            || std::fs::read_to_string(&completion).ok().as_deref() != Some(identity.as_str())
        {
            match std::fs::remove_file(&completion) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(DeployError::Other(error.to_string())),
            }
            if let Some(log) = log {
                log.info(&format!("[managed-cli] installing {package}@{version}"));
            }
            let home = root.join("installer-home");
            let tmp = home.join("tmp");
            std::fs::create_dir_all(&tmp).map_err(|e| DeployError::Other(e.to_string()))?;
            let user_config = home.join("user.npmrc");
            let global_config = home.join("global.npmrc");
            for config in [&user_config, &global_config] {
                std::fs::write(config, b"").map_err(|e| DeployError::Other(e.to_string()))?;
            }
            let output = Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("HOME", &home)
                .env("TMPDIR", &tmp)
                .env("npm_config_userconfig", &user_config)
                .env("npm_config_globalconfig", &global_config)
                .current_dir(&root)
                .arg("install")
                .arg("--prefix")
                .arg(&root)
                .arg("--no-audit")
                .arg("--no-fund")
                .arg(format!("{package}@{version}"))
                .output()
                .await
                .map_err(|e| DeployError::Spawn(format!("start npm installer: {e}")))?;
            if !output.status.success() {
                return Err(DeployError::Spawn(format!(
                    "npm install {package}@{version} failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                )));
            }
            if !bin.join(&name).is_file() {
                return Err(DeployError::Spawn(
                    "npm did not install the agent executable".into(),
                ));
            }
            std::fs::write(completion, identity).map_err(|e| DeployError::Other(e.to_string()))?;
        }
        bin
    } else {
        let expected = artifact(
            engine,
            version,
            std::env::consts::OS,
            std::env::consts::ARCH,
        )?;
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).map_err(|e| DeployError::Other(e.to_string()))?;
        let destination = bin.join(executable);
        if !destination.exists() {
            let staging = root.join(format!("{}.incoming", uuid::Uuid::new_v4()));
            if let Some(log) = log {
                log.info(&format!("[managed-cli] downloading {engine}@{version}"));
            }
            if let Err(error) = crate::services::model_download::download_with_progress(
                expected.url,
                &staging,
                executable,
                None,
            )
            .await
            {
                let _ = std::fs::remove_file(&staging);
                return Err(DeployError::Other(format!(
                    "agent artifact download: {error:#}"
                )));
            }
            let checked = tokio::task::spawn_blocking(move || {
                let result = verify_artifact(&staging, &expected);
                if result.is_err() {
                    let _ = std::fs::remove_file(&staging);
                }
                result.map(|()| staging)
            })
            .await
            .map_err(|e| DeployError::Other(e.to_string()))??;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&checked, std::fs::Permissions::from_mode(0o555))
                    .map_err(|e| DeployError::Other(e.to_string()))?;
            }
            std::fs::rename(&checked, &destination)
                .map_err(|e| DeployError::Other(e.to_string()))?;
        } else {
            tokio::task::spawn_blocking(move || verify_artifact(&destination, &expected))
                .await
                .map_err(|e| DeployError::Other(e.to_string()))??;
        }
        bin
    };
    Ok((root, bin))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_lock(path: &Path) -> std::fs::File {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .unwrap()
    }

    #[tokio::test]
    async fn concurrent_accounts_wait_for_shared_installation_then_reuse_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("install.lock");
        let first = open_lock(&path);
        acquire_install_lock(&first, std::time::Duration::from_secs(2))
            .await
            .unwrap();
        let second = open_lock(&path);
        let mut waiter = tokio::spawn(async move {
            acquire_install_lock(&second, std::time::Duration::from_secs(2))
                .await
                .unwrap();
            second
        });
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut waiter)
                .await
                .is_err()
        );
        std::fs::write(
            directory.path().join("installation-complete"),
            b"pinned-version",
        )
        .unwrap();
        drop(first);
        let second = waiter.await.unwrap();
        assert_eq!(
            std::fs::read(directory.path().join("installation-complete")).unwrap(),
            b"pinned-version"
        );
        let third = open_lock(&path);
        assert!(matches!(
            third.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(second);
        acquire_install_lock(&third, std::time::Duration::from_secs(2))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn canceled_or_expired_waiter_cannot_acquire_installation_later() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("install.lock");
        let first = open_lock(&path);
        first.try_lock().unwrap();
        let second = open_lock(&path);
        let error = acquire_install_lock(&second, std::time::Duration::from_millis(30))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timed out waiting"));
        let waiter = tokio::spawn(async move {
            acquire_install_lock(&second, std::time::Duration::from_secs(2)).await
        });
        tokio::task::yield_now().await;
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        drop(first);
        let third = open_lock(&path);
        acquire_install_lock(&third, std::time::Duration::from_secs(2))
            .await
            .unwrap();
    }

    #[test]
    fn artifact_verification_rejects_changed_content_and_symlinks() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("agent");
        std::fs::write(&file, b"abc").unwrap();
        let expected = Artifact {
            url: "https://example.invalid",
            size: 3,
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        };
        verify_artifact(&file, &expected).unwrap();
        std::fs::write(&file, b"abd").unwrap();
        assert!(verify_artifact(&file, &expected).is_err());
        #[cfg(unix)]
        {
            std::fs::write(&file, b"abc").unwrap();
            let alias = directory.path().join("alias");
            std::os::unix::fs::symlink(&file, &alias).unwrap();
            assert!(verify_artifact(&alias, &expected).is_err());
        }
        assert!(artifact("grok-build", "latest", "macos", "aarch64").is_err());
        assert!(artifact("muse-code", "1.0.3-R2198.1", "windows", "x86_64").is_err());
    }
}
