// =============================================================================
// File: util/path_safety.rs — sandbox a relative path inside a base directory
// =============================================================================
//
// Single source of truth for "resolve a manifest-supplied relative path against
// an addon root, and refuse anything that could escape it". Used by the addon
// installer (verifying ui_component bundles) and by `tentaflow-cli addon
// validate`. Keeps both call sites identical so a fix to one (e.g. new attack
// vector) protects the other.
//
// Rejected inputs:
// - absolute paths
// - any component that is `..` (syntactic check, before any IO)
// - paths whose canonicalized form lies outside the canonicalized base
// - the resolved entry being a symlink (defense in depth — canonicalize would
//   already follow it, but we want a typed error rather than silently
//   accepting a link that happens to point inside)

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum PathSafetyError {
    #[error("absolute path rejected: {0}")]
    AbsolutePathRejected(String),
    #[error("parent traversal (..) rejected: {0}")]
    ParentTraversalRejected(String),
    #[error("base canonicalize failed: {0}")]
    BaseCanonicalize(String),
    #[error("joined canonicalize failed: {0}")]
    JoinedCanonicalize(String),
    #[error("path escapes base directory: base={base}, attempted={attempted}")]
    EscapesBase { base: String, attempted: String },
    #[error("symlink metadata error: {0}")]
    SymlinkMetadata(String),
    #[error("symlink rejected: {0}")]
    SymlinkRejected(String),
}

/// Resolves `rel` against `base` and verifies the result stays within `base`.
pub fn safe_resolve(base: &Path, rel: &str) -> Result<PathBuf, PathSafetyError> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err(PathSafetyError::AbsolutePathRejected(rel.to_string()));
    }

    if rel_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(PathSafetyError::ParentTraversalRejected(rel.to_string()));
    }

    let base_canonical = base
        .canonicalize()
        .map_err(|e| PathSafetyError::BaseCanonicalize(e.to_string()))?;

    let joined = base_canonical.join(rel_path);
    let joined_canonical = joined
        .canonicalize()
        .map_err(|e| PathSafetyError::JoinedCanonicalize(format!("{rel}: {e}")))?;

    if !joined_canonical.starts_with(&base_canonical) {
        return Err(PathSafetyError::EscapesBase {
            base: base_canonical.display().to_string(),
            attempted: joined_canonical.display().to_string(),
        });
    }

    // Defense in depth — canonicalize already followed links, but we want a
    // typed rejection rather than implicit acceptance of symlinked entries.
    let metadata = std::fs::symlink_metadata(base.join(rel_path))
        .map_err(|e| PathSafetyError::SymlinkMetadata(e.to_string()))?;
    if metadata.file_type().is_symlink() {
        return Err(PathSafetyError::SymlinkRejected(rel.to_string()));
    }

    Ok(joined_canonical)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_file(dir: &Path, name: &str, content: &[u8]) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(content).unwrap();
    }

    #[test]
    fn accepts_simple_relative_file() {
        let tmp = tempfile::tempdir().unwrap();
        make_file(tmp.path(), "ok.txt", b"x");
        let p = safe_resolve(tmp.path(), "ok.txt").expect("ok");
        assert!(p.ends_with("ok.txt"));
    }

    #[test]
    fn rejects_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();
        match safe_resolve(tmp.path(), "/etc/passwd") {
            Err(PathSafetyError::AbsolutePathRejected(_)) => {}
            other => panic!("expected AbsolutePathRejected, got {other:?}"),
        }
    }

    #[test]
    fn rejects_parent_traversal_syntactic() {
        let tmp = tempfile::tempdir().unwrap();
        match safe_resolve(tmp.path(), "../../etc/passwd") {
            Err(PathSafetyError::ParentTraversalRejected(_)) => {}
            other => panic!("expected ParentTraversalRejected, got {other:?}"),
        }
    }

    #[test]
    fn rejects_symlink_inside_base() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        make_file(outside.path(), "target.txt", b"secret");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path().join("target.txt"), tmp.path().join("link"))
                .unwrap();
            match safe_resolve(tmp.path(), "link") {
                Err(PathSafetyError::SymlinkRejected(_))
                | Err(PathSafetyError::EscapesBase { .. }) => {}
                other => panic!("expected SymlinkRejected/EscapesBase, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_missing_target() {
        let tmp = tempfile::tempdir().unwrap();
        match safe_resolve(tmp.path(), "does-not-exist") {
            Err(PathSafetyError::JoinedCanonicalize(_)) => {}
            other => panic!("expected JoinedCanonicalize, got {other:?}"),
        }
    }
}
