// ===== File: code_studio/paths.rs — directory layout of a workspace on its owner node =====
//
// Metadata paths derive from workspace IDs. Session files use either a
// managed worktree or an administrator-approved immutable location binding;
// a session request never supplies its own host path.
//
// Layout (§5.4 of the plan):
//
//   <data>/code-studio/<workspace_id>/
//       repo/                        reference tree + git metadata (NEVER mounted)
//       worktrees/<session_id>/       working worktree of a session
//       worktrees/<session_id>_int/   integration worktree of a merge
//       workspace.db                  runtime state of the owner node
//       artifacts/<aa>/<sha256>
//       vectors/
//       toolchain-cache/base/         trusted read-only cache
//       toolchain-cache/ov/<sid>/     per-session overlay
//       tmp/<session_id>/

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

use crate::paths::{category_dir, StorageCategory};

/// Accepts exactly what `uuid::Uuid::new_v4().to_string()` produces plus the
/// slug-ish ids used in tests: lowercase hex/alphanumerics and dashes. It is
/// deliberately narrower than "no traversal" — a whitelist cannot be defeated
/// by an encoding trick the way a blacklist can.
pub fn validate_workspace_id(workspace_id: &str) -> Result<()> {
    if workspace_id.is_empty() || workspace_id.len() > 64 {
        return Err(anyhow!("invalid workspace id"));
    }
    if !workspace_id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(anyhow!("invalid workspace id"));
    }
    Ok(())
}

/// Same alphabet as a workspace id. Session ids reach the filesystem through
/// worktree and tmp directories, so they need the identical guard.
pub fn validate_session_id(session_id: &str) -> Result<()> {
    validate_workspace_id(session_id).map_err(|_| anyhow!("invalid session id"))
}

/// Root of every workspace of this node. Follows the Storage settings, so
/// moving the Data category moves Code Studio with it.
pub fn root_dir() -> PathBuf {
    category_dir(StorageCategory::Data).join("code-studio")
}

pub fn workspace_dir(workspace_id: &str) -> Result<PathBuf> {
    validate_workspace_id(workspace_id)?;
    Ok(root_dir().join(workspace_id))
}

/// The reference clone. It holds the git metadata and is NEVER mounted into a
/// sandbox — sessions get worktrees, and git runs in the broker outside them.
pub fn repo_dir(workspace_id: &str) -> Result<PathBuf> {
    Ok(workspace_dir(workspace_id)?.join("repo"))
}

pub fn workspace_db_path(workspace_id: &str) -> Result<PathBuf> {
    Ok(workspace_dir(workspace_id)?.join("workspace.db"))
}

pub fn worktrees_dir(workspace_id: &str) -> Result<PathBuf> {
    Ok(workspace_dir(workspace_id)?.join("worktrees"))
}

pub fn session_worktree_dir(workspace_id: &str, session_id: &str) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    if let Some(path) = super::location::resolve(&workspace_dir(workspace_id)?)? {
        return Ok(path);
    }
    Ok(worktrees_dir(workspace_id)?.join(session_id))
}

/// Worktree a merge is prepared in, detached from the target branch (§11.6).
/// Named after the session so an interrupted merge is attributable, and kept
/// separate from the working worktree because a conflict leaves it `held`.
pub fn integration_worktree_dir(workspace_id: &str, session_id: &str) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    // `_` cannot appear in a session id, so this name can never be the WORKING
    // worktree of another session — which `<id>-int` could be, for a session
    // literally called `<id>-int`.
    Ok(worktrees_dir(workspace_id)?.join(format!("{session_id}_int")))
}

pub fn artifacts_dir(workspace_id: &str) -> Result<PathBuf> {
    Ok(workspace_dir(workspace_id)?.join("artifacts"))
}

/// Content-addressed artifact path, sharded by the first two hex characters so
/// a single directory never holds hundreds of thousands of entries.
pub fn artifact_path(workspace_id: &str, sha256_hex: &str) -> Result<PathBuf> {
    if sha256_hex.len() != 64 || !sha256_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(anyhow!("artifact digest must be 64 hex characters"));
    }
    let sha = sha256_hex.to_ascii_lowercase();
    Ok(artifacts_dir(workspace_id)?.join(&sha[..2]).join(&sha))
}

pub fn vectors_dir(workspace_id: &str) -> Result<PathBuf> {
    Ok(workspace_dir(workspace_id)?.join("vectors"))
}

pub fn toolchain_base_dir(workspace_id: &str) -> Result<PathBuf> {
    Ok(workspace_dir(workspace_id)?
        .join("toolchain-cache")
        .join("base"))
}

pub fn toolchain_overlay_dir(workspace_id: &str, session_id: &str) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    Ok(workspace_dir(workspace_id)?
        .join("toolchain-cache")
        .join("ov")
        .join(session_id))
}

pub fn session_tmp_dir(workspace_id: &str, session_id: &str) -> Result<PathBuf> {
    validate_session_id(session_id)?;
    Ok(workspace_dir(workspace_id)?.join("tmp").join(session_id))
}

/// Creates the directories a workspace needs before anything is written into
/// it. Idempotent, so a resumed provisioning saga can call it again.
pub fn create_workspace_layout(workspace_id: &str) -> Result<PathBuf> {
    let root = workspace_dir(workspace_id)?;
    for dir in [
        root.clone(),
        repo_dir(workspace_id)?,
        worktrees_dir(workspace_id)?,
        artifacts_dir(workspace_id)?,
        vectors_dir(workspace_id)?,
        toolchain_base_dir(workspace_id)?,
        root.join("toolchain-cache").join("ov"),
        root.join("tmp"),
    ] {
        std::fs::create_dir_all(&dir)
            .map_err(|e| anyhow!("code_studio: create {}: {e}", dir.display()))?;
    }
    restrict_permissions(&root)?;
    Ok(root)
}

/// The workspace tree holds source code and a git config carrying credential
/// policy; on unix it is owner-only. On other platforms the parent data
/// directory's ACL governs, and this is a no-op rather than a false promise.
fn restrict_permissions(_dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(_dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| anyhow!("code_studio: chmod {}: {e}", _dir.display()))?;
    }
    Ok(())
}

/// Serialises every test that redirects the Data category, across ALL modules.
///
/// `crate::paths::set_category_override` is process-global, so a mutex declared
/// inside one test module only protects that module against itself: another
/// module's test could clear the override mid-run and leave the first one
/// resolving paths under a temporary directory that no longer exists. That is
/// not theoretical — it produced a `git init` failing to write its own config.
#[cfg(test)]
pub(crate) fn test_data_dir_guard() -> std::sync::MutexGuard<'static, ()> {
    static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
    GUARD
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_that_could_escape_the_workspace_root_are_refused() {
        for bad in [
            "",
            "..",
            "../etc",
            "a/b",
            "a\\b",
            "UPPER",
            "with space",
            "null\0byte",
            &"x".repeat(65),
        ] {
            assert!(
                validate_workspace_id(bad).is_err(),
                "accepted {bad:?} as a workspace id"
            );
            assert!(
                validate_session_id(bad).is_err(),
                "accepted {bad:?} as a session id"
            );
        }
        assert!(validate_workspace_id("9f2a1c4b-0e5d-4a77-9c31-8a2b6d4e1f00").is_ok());
    }

    #[test]
    fn every_path_helper_stays_under_the_workspace_directory() {
        let id = "9f2a1c4b-0e5d-4a77-9c31-8a2b6d4e1f00";
        let root = workspace_dir(id).unwrap();
        for path in [
            repo_dir(id).unwrap(),
            workspace_db_path(id).unwrap(),
            session_worktree_dir(id, "s-1").unwrap(),
            integration_worktree_dir(id, "s-1").unwrap(),
            artifacts_dir(id).unwrap(),
            vectors_dir(id).unwrap(),
            toolchain_base_dir(id).unwrap(),
            toolchain_overlay_dir(id, "s-1").unwrap(),
            session_tmp_dir(id, "s-1").unwrap(),
        ] {
            assert!(
                path.starts_with(&root),
                "{} escaped {}",
                path.display(),
                root.display()
            );
        }
    }

    #[test]
    fn the_integration_worktree_never_collides_with_the_working_one() {
        let id = "9f2a1c4b-0e5d-4a77-9c31-8a2b6d4e1f00";
        assert_ne!(
            session_worktree_dir(id, "s-1").unwrap(),
            integration_worktree_dir(id, "s-1").unwrap()
        );
    }

    #[test]
    fn artifact_paths_are_sharded_and_reject_a_non_digest() {
        let id = "9f2a1c4b-0e5d-4a77-9c31-8a2b6d4e1f00";
        let digest = "a".repeat(64);
        let path = artifact_path(id, &digest).unwrap();
        assert!(path.starts_with(artifacts_dir(id).unwrap().join("aa")));

        for bad in ["", "abc", &"z".repeat(64), &"a".repeat(63)] {
            assert!(artifact_path(id, bad).is_err(), "accepted digest {bad:?}");
        }
    }
}
