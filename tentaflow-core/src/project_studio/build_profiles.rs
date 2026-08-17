// ===== File: project_studio/build_profiles.rs — build/test recipes for code sources (F3) =====
//
// One profile per git/zip source (`source_id` is UNIQUE): the toolchain plus
// the install/test commands a unit-test item executes inside the sandbox. The
// commands are arbitrary shell, so the runner refuses to execute them unless it
// reports `isolated: true` — see `executor/build_profile.py`.
//
// `detect_toolchain` reads the ingested file list of a source (never the
// filesystem) and proposes a starting recipe, so the UI can offer a filled form
// instead of an empty one.

use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension};

use super::models::BuildProfileRecord;
use crate::db::DbPool;

pub const TOOLCHAINS: &[&str] = &["python", "node", "dotnet", "jvm", "rust", "go"];

/// Hard bound on a single command; mirrors `MAX_CMD_CHARS` in the runner.
pub const MAX_CMD_CHARS: usize = 4096;

fn read_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio build profiles read: {e}")
}

fn write_err(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("project_studio build profiles write: {e}")
}

const PROFILE_COLS: &str =
    "profile_id, source_id, toolchain, base_image, install_cmd, test_cmd, workdir, proposed_by";

fn read_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<BuildProfileRecord> {
    Ok(BuildProfileRecord {
        profile_id: row.get(0)?,
        source_id: row.get(1)?,
        toolchain: row.get(2)?,
        base_image: row.get(3)?,
        install_cmd: row.get(4)?,
        test_cmd: row.get(5)?,
        workdir: row.get(6)?,
        proposed_by: row.get(7)?,
    })
}

pub fn get(pool: &DbPool, source_id: &str) -> Result<Option<BuildProfileRecord>> {
    let conn = pool.read().map_err(read_err)?;
    conn.query_row(
        &format!("SELECT {PROFILE_COLS} FROM build_profiles WHERE source_id = ?1"),
        params![source_id],
        read_profile,
    )
    .optional()
    .map_err(Into::into)
}

/// Upserts the single profile of a source. Returns its id.
#[allow(clippy::too_many_arguments)]
pub fn upsert(
    pool: &DbPool,
    source_id: &str,
    toolchain: &str,
    base_image: &str,
    install_cmd: &str,
    test_cmd: &str,
    workdir: &str,
    proposed_by: &str,
) -> Result<String> {
    let conn = pool.write().map_err(write_err)?;
    let existing: Option<String> = conn
        .query_row(
            "SELECT profile_id FROM build_profiles WHERE source_id = ?1",
            params![source_id],
            |row| row.get(0),
        )
        .optional()?;
    match existing {
        Some(profile_id) => {
            conn.execute(
                "UPDATE build_profiles SET toolchain = ?1, base_image = ?2, install_cmd = ?3, \
                    test_cmd = ?4, workdir = ?5, proposed_by = ?6, updated_at = datetime('now') \
                 WHERE profile_id = ?7",
                params![
                    toolchain,
                    base_image,
                    install_cmd,
                    test_cmd,
                    workdir,
                    proposed_by,
                    profile_id
                ],
            )?;
            Ok(profile_id)
        }
        None => {
            let profile_id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO build_profiles (profile_id, source_id, toolchain, base_image, \
                    install_cmd, test_cmd, workdir, proposed_by) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    profile_id,
                    source_id,
                    toolchain,
                    base_image,
                    install_cmd,
                    test_cmd,
                    workdir,
                    proposed_by
                ],
            )?;
            Ok(profile_id)
        }
    }
}

pub fn delete_for_source(pool: &DbPool, source_id: &str) -> Result<()> {
    let conn = pool.write().map_err(write_err)?;
    conn.execute(
        "DELETE FROM build_profiles WHERE source_id = ?1",
        params![source_id],
    )?;
    Ok(())
}

/// Validates and normalizes a caller-supplied recipe.
pub fn validate(toolchain: &str, install_cmd: &str, test_cmd: &str, workdir: &str) -> Result<()> {
    if !TOOLCHAINS.contains(&toolchain) {
        return Err(anyhow!("unknown toolchain '{toolchain}'"));
    }
    if test_cmd.trim().is_empty() {
        return Err(anyhow!("test_cmd is required"));
    }
    for cmd in [install_cmd, test_cmd] {
        if cmd.len() > MAX_CMD_CHARS {
            return Err(anyhow!("command exceeds {MAX_CMD_CHARS} characters"));
        }
    }
    // The workdir is a path INSIDE the source snapshot; an absolute path or a
    // parent traversal would escape the mounted tree.
    if workdir.starts_with('/')
        || workdir.starts_with('\\')
        || workdir.split(['/', '\\']).any(|p| p == "..")
    {
        return Err(anyhow!(
            "workdir must be a relative path inside the source"
        ));
    }
    Ok(())
}

/// A proposed recipe derived from the file list of a code source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedProfile {
    pub toolchain: &'static str,
    pub base_image: &'static str,
    pub install_cmd: String,
    pub test_cmd: &'static str,
    /// Directory (relative to the source root) holding the manifest file.
    pub workdir: String,
}

/// Proposes a build recipe from the source's ingested paths. The SHALLOWEST
/// manifest wins — a repo root `package.json` beats one under `examples/`.
pub fn detect_toolchain(paths: &[String]) -> Option<ProposedProfile> {
    // (file name, toolchain, base image, install, test)
    const MARKERS: &[(&str, &str, &str, &str, &str)] = &[
        (
            "Cargo.toml",
            "rust",
            "rust:1-slim",
            "cargo fetch --locked",
            "cargo test --offline",
        ),
        (
            "go.mod",
            "go",
            "golang:1-bookworm",
            "go mod download",
            "go test ./...",
        ),
        (
            "pom.xml",
            "jvm",
            "maven:3-eclipse-temurin-21",
            "mvn -o -B dependency:go-offline",
            "mvn -o -B test",
        ),
        (
            "build.gradle",
            "jvm",
            "gradle:8-jdk21",
            "gradle --offline dependencies",
            "gradle --offline test",
        ),
        (
            "package.json",
            "node",
            "node:22-bookworm-slim",
            "npm ci --no-audit --no-fund",
            "npm test",
        ),
        (
            "pyproject.toml",
            "python",
            "python:3.12-slim",
            "pip install --no-input .",
            "pytest -q",
        ),
        (
            "requirements.txt",
            "python",
            "python:3.12-slim",
            "pip install --no-input -r requirements.txt",
            "pytest -q",
        ),
    ];

    let mut best: Option<(usize, ProposedProfile)> = None;
    for path in paths {
        let normalized = path.replace('\\', "/");
        let (dir, file) = match normalized.rsplit_once('/') {
            Some((dir, file)) => (dir.to_string(), file.to_string()),
            None => (String::new(), normalized.clone()),
        };
        let depth = if dir.is_empty() {
            0
        } else {
            dir.split('/').count()
        };
        let matched = MARKERS
            .iter()
            .find(|(marker, ..)| *marker == file)
            .or_else(|| {
                // .csproj / .fsproj are name patterns, not fixed file names.
                (file.ends_with(".csproj") || file.ends_with(".fsproj")).then_some(&(
                    "",
                    "dotnet",
                    "mcr.microsoft.com/dotnet/sdk:9.0",
                    "dotnet restore",
                    "dotnet test --no-restore",
                ))
            });
        let Some((_, toolchain, base_image, install, test)) = matched else {
            continue;
        };
        if best.as_ref().is_some_and(|(best_depth, _)| depth >= *best_depth) {
            continue;
        }
        best = Some((
            depth,
            ProposedProfile {
                toolchain,
                base_image,
                install_cmd: (*install).to_string(),
                test_cmd: test,
                workdir: dir,
            },
        ));
    }
    best.map(|(_, profile)| profile)
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn detect_toolchain_prefers_the_shallowest_manifest() {
        let paths = vec![
            "examples/demo/package.json".to_string(),
            "src/main.rs".to_string(),
            "Cargo.toml".to_string(),
        ];
        let proposal = detect_toolchain(&paths).expect("proposal");
        assert_eq!(proposal.toolchain, "rust");
        assert_eq!(proposal.workdir, "");

        let node = detect_toolchain(&["web/app/package.json".to_string()]).expect("node");
        assert_eq!(node.toolchain, "node");
        assert_eq!(node.workdir, "web/app");

        let dotnet = detect_toolchain(&["Api/Api.csproj".to_string()]).expect("dotnet");
        assert_eq!(dotnet.toolchain, "dotnet");
        assert_eq!(dotnet.workdir, "Api");

        assert!(detect_toolchain(&["README.md".to_string()]).is_none());
    }

    #[test]
    fn validate_rejects_escaping_workdir_and_unknown_toolchain() {
        assert!(validate("rust", "", "cargo test", "crates/core").is_ok());
        assert!(validate("brainfuck", "", "run", "").is_err());
        assert!(validate("rust", "", "", "").is_err());
        assert!(validate("rust", "", "cargo test", "/etc").is_err());
        assert!(validate("rust", "", "cargo test", "../../etc").is_err());
        assert!(validate("rust", &"x".repeat(MAX_CMD_CHARS + 1), "cargo test", "").is_err());
    }
}
