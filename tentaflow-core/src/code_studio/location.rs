// ============ File: location.rs — Bind administrator-approved host projects without owning their files. ============

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
struct Binding {
    path: PathBuf,
    device: u64,
    inode: u64,
}

fn identity(path: &Path) -> Result<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            bail!("project must be a real directory");
        }
        Ok((meta.dev(), meta.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        bail!("existing-directory sandbox is not validated on this platform")
    }
}

pub fn validate(raw: &str) -> Result<PathBuf> {
    let path = Path::new(raw);
    if !path.is_absolute() {
        bail!("project directory must be absolute");
    }
    let canonical = path
        .canonicalize()
        .context("project directory is not accessible")?;
    if canonical.parent().is_none() {
        bail!("filesystem root cannot be a project");
    }
    for protected in [
        crate::paths::keys_dir(),
        crate::paths::data_dir(),
        crate::paths::sync_dir(),
    ] {
        let protected = protected.canonicalize().unwrap_or(protected);
        if canonical.starts_with(&protected) || protected.starts_with(&canonical) {
            bail!("project overlaps private TentaFlow storage; relocate storage before registering this directory");
        }
    }
    if std::env::current_exe()?
        .canonicalize()?
        .starts_with(&canonical)
    {
        bail!("run TentaFlow from an installation outside this project before granting write access to its source");
    }
    let private = super::paths::root_dir();
    let private = private.canonicalize().unwrap_or(private);
    if canonical.starts_with(&private) || private.starts_with(&canonical) {
        bail!(
            "project overlaps private Code Studio storage; move storage outside the project first"
        );
    }
    identity(&canonical)?;
    validate_git_metadata(&canonical)?;
    super::process_sandbox::validate_workspace_tree(&canonical)?;
    Ok(canonical)
}

pub fn validate_git_metadata(project: &Path) -> Result<()> {
    let git = project.join(".git");
    let metadata =
        std::fs::symlink_metadata(&git).context("existing project needs its own .git directory")?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "linked Git worktrees and external Git metadata are not supported for direct projects"
        );
    }
    // The Git broker runs outside the CLI sandbox, so metadata must not redirect it.
    let mut directories = vec![git.clone()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                directories.push(entry.path());
            } else if !kind.is_file() {
                bail!("direct projects cannot contain redirected or special Git metadata");
            }
        }
    }
    super::process_sandbox::validate_workspace_tree(&git)?;
    for name in [
        "commondir",
        "objects/info/alternates",
        "objects/info/http-alternates",
    ] {
        if std::fs::symlink_metadata(git.join(name)).is_ok() {
            bail!("direct projects cannot reference external Git metadata ({name})");
        }
    }
    for name in ["config", "config.worktree"] {
        let content = match std::fs::read_to_string(git.join(name)) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if content.lines().any(|line| {
            line.trim_start_matches('\u{feff}')
                .trim_start()
                .strip_prefix('[')
                .is_some_and(|section| {
                    section
                        .trim_start()
                        .to_ascii_lowercase()
                        .starts_with("include")
                })
        }) {
            bail!("direct projects cannot include external Git configuration");
        }
    }
    Ok(())
}

pub fn bind(root: &Path, path: &Path) -> Result<()> {
    let path = validate(path.to_str().context("project path must be UTF-8")?)?;
    let (device, inode) = identity(&path)?;
    let target = root.join("location.json");
    if let Some(existing) = resolve(root)? {
        if existing != path {
            bail!("project directory binding is immutable");
        }
        return Ok(());
    }
    let encoded = serde_json::to_vec(&Binding {
        path,
        device,
        inode,
    })?;
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    Ok(())
}

pub fn resolve(root: &Path) -> Result<Option<PathBuf>> {
    let bytes = match std::fs::read(root.join("location.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let binding: Binding = serde_json::from_slice(&bytes)?;
    if binding.path.canonicalize()? != binding.path
        || identity(&binding.path)? != (binding.device, binding.inode)
    {
        bail!("approved project directory was replaced; administrator must re-register it");
    }
    Ok(Some(binding.path))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn binding_detects_replaced_directory_and_preserves_source() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let project = source.path().join("project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::write(project.join("code.txt"), "original").unwrap();
        bind(root.path(), &project).unwrap();
        assert_eq!(
            resolve(root.path()).unwrap(),
            Some(project.canonicalize().unwrap())
        );
        std::fs::rename(&project, source.path().join("previous")).unwrap();
        std::fs::create_dir(&project).unwrap();
        assert!(resolve(root.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(source.path().join("previous/code.txt")).unwrap(),
            "original"
        );
    }

    #[test]
    fn rejects_git_metadata_symlinks_before_reading_configuration() {
        let source = tempfile::tempdir().unwrap();
        let project = source.path().join("project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let foreign = source.path().join("foreign-config");
        std::fs::write(&foreign, "[core]\nrepositoryformatversion = 0\n").unwrap();
        std::os::unix::fs::symlink(&foreign, project.join(".git/config")).unwrap();
        let error = validate_git_metadata(&project).unwrap_err();
        assert!(error
            .to_string()
            .contains("redirected or special Git metadata"));
        std::fs::remove_file(project.join(".git/config")).unwrap();
        std::os::unix::fs::symlink(source.path(), project.join(".git/objects")).unwrap();
        assert!(validate_git_metadata(&project).is_err());
    }

    #[test]
    fn rejects_external_metadata_and_hardlinks() {
        let source = tempfile::tempdir().unwrap();
        let project = source.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join(".git"), "gitdir: /other").unwrap();
        assert!(validate(project.to_str().unwrap()).is_err());
        std::fs::remove_file(project.join(".git")).unwrap();
        std::fs::create_dir(project.join(".git")).unwrap();
        std::fs::write(project.join(".git/commondir"), "/other/.git").unwrap();
        assert!(validate(project.to_str().unwrap()).is_err());
        std::fs::remove_file(project.join(".git/commondir")).unwrap();
        std::fs::write(
            project.join(".git/config"),
            "[include]\npath = /other/config\n",
        )
        .unwrap();
        assert!(validate(project.to_str().unwrap()).is_err());
        std::fs::remove_file(project.join(".git/config")).unwrap();
        std::fs::write(source.path().join("secret"), "private").unwrap();
        std::fs::hard_link(source.path().join("secret"), project.join("alias")).unwrap();
        assert!(validate(project.to_str().unwrap()).is_err());
    }
}
