// ===== File: hf.rs — `forge pull`: download models from the HuggingFace Hub =====
// Resolves an HF repo to its files via the Hub tree API, downloads a single
// GGUF (explicit or auto-selected) or a full safetensors snapshot directory,
// resumes partial downloads via HTTP Range, and verifies size / LFS sha256.
// The snapshot layout mirrors an HF checkout so the existing loader (config.json
// + tokenizer.json + *.safetensors) reads it directly.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

const HF_BASE: &str = "https://huggingface.co";
const USER_AGENT: &str = concat!("forge/", env!("CARGO_PKG_VERSION"));

/// One entry of the HF repo tree. LFS files (the large weights) carry a
/// `lfs.oid` sha256; small git-tracked files only have a git blob sha1 (`oid`),
/// which is not a content hash, so those are verified by byte length instead.
#[derive(Debug, Deserialize)]
struct TreeEntry {
    #[serde(rename = "type")]
    kind: String,
    path: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    lfs: Option<Lfs>,
}

#[derive(Debug, Deserialize)]
struct Lfs {
    oid: String,
    size: u64,
}

impl TreeEntry {
    /// Content sha256 (LFS oid) if the file is LFS-tracked.
    fn sha256(&self) -> Option<&str> {
        self.lfs.as_ref().map(|l| l.oid.as_str())
    }
    /// Real byte length: LFS entries report the pointer size in `size`, the
    /// actual object size in `lfs.size`.
    fn byte_len(&self) -> u64 {
        self.lfs.as_ref().map(|l| l.size).unwrap_or(self.size)
    }
}

/// Files a safetensors snapshot needs for the loader plus tokenizer/template.
/// Everything else in the repo (READMEs, `.bin`/`.pth`/`.onnx` duplicates,
/// images, other-quant GGUFs) is skipped.
fn snapshot_wanted(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.ends_with(".safetensors") {
        return true;
    }
    matches!(
        name,
        "config.json"
            | "generation_config.json"
            | "model.safetensors.index.json"
            | "tokenizer.json"
            | "tokenizer_config.json"
            | "tokenizer.model"
            | "special_tokens_map.json"
            | "added_tokens.json"
            | "vocab.json"
            | "merges.txt"
            | "chat_template.jinja"
            | "chat_template.json"
            | "preprocessor_config.json"
            | "modules.json"
            | "sentence_bert_config.json"
    ) || path.ends_with("1_Pooling/config.json")
}

/// Default download cache when `--dir` is not given:
/// `$XDG_CACHE_HOME/forge/hub/<repo-flattened>` (or `~/.cache/...`).
fn default_dest(repo: &str) -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("forge").join("hub").join(repo.replace('/', "--"))
}

/// Fetch the recursive repo tree, following `Link: rel="next"` pagination.
async fn list_tree(
    client: &reqwest::Client,
    repo: &str,
    revision: &str,
    token: Option<&str>,
) -> Result<Vec<TreeEntry>> {
    let mut url = format!("{HF_BASE}/api/models/{repo}/tree/{revision}?recursive=true&expand=true");
    let mut entries = Vec::new();
    loop {
        let mut req = client.get(&url);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        let resp = req.send().await.context("query HF tree API")?;
        let status = resp.status();
        if !status.is_success() {
            auth_hint(status.as_u16(), repo, "list files")?;
            bail!("HF tree API for {repo}@{revision} returned HTTP {status}");
        }
        let next = resp
            .headers()
            .get(reqwest::header::LINK)
            .and_then(|v| v.to_str().ok())
            .and_then(parse_next_link);
        let page: Vec<TreeEntry> = resp.json().await.context("parse HF tree JSON")?;
        entries.extend(page);
        match next {
            Some(n) => {
                url = if n.starts_with("http") {
                    n
                } else {
                    format!("{HF_BASE}{n}")
                }
            }
            None => break,
        }
    }
    Ok(entries)
}

/// Extract the `rel="next"` target from an RFC5988 `Link` header.
fn parse_next_link(header: &str) -> Option<String> {
    for part in header.split(',') {
        if part.contains("rel=\"next\"") {
            let start = part.find('<')?;
            let end = part.find('>')?;
            return Some(part[start + 1..end].to_string());
        }
    }
    None
}

/// Map 401/403 to an actionable error; other statuses fall through.
fn auth_hint(status: u16, repo: &str, action: &str) -> Result<()> {
    match status {
        401 => bail!(
            "{action} for {repo}: HTTP 401 Unauthorized. This repo needs authentication — \
             pass --token <hf_token> or set HF_TOKEN (create one at \
             https://huggingface.co/settings/tokens)."
        ),
        403 => bail!(
            "{action} for {repo}: HTTP 403 Forbidden. This is a gated/private repo — accept its \
             license at https://huggingface.co/{repo} and use a --token / HF_TOKEN that has access."
        ),
        _ => Ok(()),
    }
}

/// Human-readable megabytes.
fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// One file to fetch: where it lives on the Hub and how to verify it.
struct RemoteFile<'a> {
    repo: &'a str,
    revision: &'a str,
    remote_path: &'a str,
    token: Option<&'a str>,
    expected_size: u64,
    expected_sha256: Option<&'a str>,
}

/// Download one file with resume + progress + integrity. Streams to `<dest>.part`
/// then atomically renames. Resumes from an existing `.part` via HTTP Range.
async fn download_file(client: &reqwest::Client, f: &RemoteFile<'_>, dest: &Path) -> Result<()> {
    let RemoteFile {
        repo,
        revision,
        remote_path,
        token,
        expected_size,
        expected_sha256,
    } = *f;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let label = dest
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(remote_path);

    // A complete destination of the right size (and hash, if known) is kept.
    if let Ok(meta) = std::fs::metadata(dest) {
        if expected_size == 0 || meta.len() == expected_size {
            if let Some(sha) = expected_sha256 {
                if sha256_file(dest).await? == sha {
                    eprintln!(
                        "{label}: already present ({:.1} MB), skipping",
                        mb(meta.len())
                    );
                    return Ok(());
                }
            } else {
                eprintln!(
                    "{label}: already present ({:.1} MB), skipping",
                    mb(meta.len())
                );
                return Ok(());
            }
        }
    }

    let part = PathBuf::from(format!("{}.part", dest.display()));
    let mut resume_from = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    if expected_size != 0 && resume_from > expected_size {
        // Corrupt / stale partial larger than the target: restart cleanly.
        let _ = std::fs::remove_file(&part);
        resume_from = 0;
    }

    let url = format!("{HF_BASE}/{repo}/resolve/{revision}/{remote_path}");
    let mut req = client.get(&url);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    if resume_from > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={resume_from}-"));
    }
    let resp = req.send().await.with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    auth_hint(status.as_u16(), repo, "download")?;
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        // The partial already covers the whole object: finalize it.
        finalize(&part, dest, expected_size, expected_sha256).await?;
        eprintln!("{label}: complete ({:.1} MB)", mb(expected_size));
        return Ok(());
    }
    if !status.is_success() {
        bail!("download {remote_path} from {repo}@{revision}: HTTP {status}");
    }

    // If we asked for a range but the server ignored it (200 not 206), it will
    // resend from byte 0 — restart the part file rather than corrupt it.
    let resuming = resume_from > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;
    if resume_from > 0 && !resuming {
        resume_from = 0;
    }
    let remaining = resp.content_length().unwrap_or(0);
    let total = if resuming {
        resume_from + remaining
    } else if expected_size != 0 {
        expected_size
    } else {
        remaining
    };

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(!resuming)
        .open(&part)
        .await
        .with_context(|| format!("open {}", part.display()))?;
    if resuming {
        file.seek(std::io::SeekFrom::Start(resume_from)).await?;
    }

    let mut resp = resp;
    let mut done = resume_from;
    let start = Instant::now();
    let mut last_print = Instant::now();
    if resuming {
        eprintln!("{label}: resuming at {:.1} MB", mb(resume_from));
    }
    while let Some(chunk) = resp.chunk().await.context("read response chunk")? {
        file.write_all(&chunk).await.context("write .part")?;
        done += chunk.len() as u64;
        if last_print.elapsed().as_millis() >= 200 {
            print_progress(label, done, total, start);
            last_print = Instant::now();
        }
    }
    file.flush().await?;
    drop(file);
    print_progress(label, done, total, start);
    eprintln!();

    finalize(&part, dest, expected_size, expected_sha256).await
}

/// Verify the finished `.part` against the expected size / sha256 and rename.
async fn finalize(
    part: &Path,
    dest: &Path,
    expected_size: u64,
    expected_sha256: Option<&str>,
) -> Result<()> {
    let got = std::fs::metadata(part)
        .with_context(|| format!("stat {}", part.display()))?
        .len();
    if expected_size != 0 && got != expected_size {
        bail!(
            "{}: size mismatch after download (got {got} bytes, expected {expected_size}); \
             re-run to resume",
            part.display()
        );
    }
    if let Some(sha) = expected_sha256 {
        let actual = sha256_file(part).await?;
        if actual != sha {
            let _ = std::fs::remove_file(part);
            bail!(
                "{}: sha256 mismatch (got {actual}, expected {sha}); removed partial, re-run",
                part.display()
            );
        }
    }
    std::fs::rename(part, dest)
        .with_context(|| format!("rename {} -> {}", part.display(), dest.display()))?;
    Ok(())
}

fn print_progress(label: &str, done: u64, total: u64, start: Instant) {
    let secs = start.elapsed().as_secs_f64().max(1e-6);
    let speed = mb(done) / secs;
    if total > 0 {
        let pct = (done as f64 / total as f64) * 100.0;
        eprint!(
            "\r{label}: {:.1}/{:.1} MB ({pct:.0}%) {speed:.1} MB/s   ",
            mb(done),
            mb(total)
        );
    } else {
        eprint!("\r{label}: {:.1} MB {speed:.1} MB/s   ", mb(done));
    }
    std::io::stderr().flush().ok();
}

/// Streaming sha256 of a file (weights are multi-GB — never buffer whole).
async fn sha256_file(path: &Path) -> Result<String> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<String> {
        let mut file =
            std::fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
        let mut hasher = Sha256::new();
        std::io::copy(&mut file, &mut hasher).context("hash file")?;
        Ok(format!("{:x}", hasher.finalize()))
    })
    .await
    .context("sha256 task")?
}

/// Resolve `--file` against the GGUF entries: exact path, basename, or a
/// case-insensitive match. Errors list the available GGUFs.
fn match_gguf<'a>(ggufs: &'a [&'a TreeEntry], want: &str) -> Result<&'a TreeEntry> {
    let lw = want.to_ascii_lowercase();
    let hit = ggufs.iter().find(|e| {
        let base = e.path.rsplit('/').next().unwrap_or(&e.path);
        e.path == want || base == want || base.to_ascii_lowercase() == lw
    });
    match hit {
        Some(e) => Ok(e),
        None => {
            let list = ggufs
                .iter()
                .map(|e| format!("  {}  ({:.1} MB)", e.path, mb(e.byte_len())))
                .collect::<Vec<_>>()
                .join("\n");
            bail!("--file '{want}' not found. Available GGUF files:\n{list}");
        }
    }
}

/// Auto-select among multiple GGUFs: prefer a Q4_K_M quant, else require --file.
fn auto_gguf<'a>(ggufs: &'a [&'a TreeEntry]) -> Result<&'a TreeEntry> {
    if let Some(e) = ggufs
        .iter()
        .find(|e| e.path.to_ascii_lowercase().contains("q4_k_m"))
    {
        eprintln!(
            "multiple quants found; selecting default Q4_K_M: {}",
            e.path
        );
        return Ok(e);
    }
    let list = ggufs
        .iter()
        .map(|e| format!("  {}  ({:.1} MB)", e.path, mb(e.byte_len())))
        .collect::<Vec<_>>()
        .join("\n");
    bail!("repo has multiple GGUF quants and no Q4_K_M default; choose one with --file:\n{list}")
}

/// Entry point for `forge pull`. Returns the final path to hand to `forge run`.
pub async fn pull(
    repo: String,
    file: Option<String>,
    revision: String,
    token: Option<String>,
    dir: Option<PathBuf>,
) -> Result<PathBuf> {
    let token = token
        .or_else(|| std::env::var("HF_TOKEN").ok())
        .filter(|t| !t.is_empty());
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .context("build HTTP client")?;

    eprintln!("resolving {repo}@{revision} on the HuggingFace Hub…");
    let tree = list_tree(&client, &repo, &revision, token.as_deref()).await?;
    let files: Vec<&TreeEntry> = tree.iter().filter(|e| e.kind == "file").collect();
    if files.is_empty() {
        bail!("{repo}@{revision} has no files");
    }
    let ggufs: Vec<&TreeEntry> = files
        .iter()
        .copied()
        .filter(|e| e.path.to_ascii_lowercase().ends_with(".gguf"))
        .collect();

    let dest_dir = dir.unwrap_or_else(|| default_dest(&repo));
    std::fs::create_dir_all(&dest_dir).with_context(|| format!("create {}", dest_dir.display()))?;

    // GGUF repo → download a single quant file. Otherwise a safetensors snapshot.
    if !ggufs.is_empty() {
        let entry = match (&file, ggufs.len()) {
            (Some(f), _) => match_gguf(&ggufs, f)?,
            (None, 1) => ggufs[0],
            (None, _) => auto_gguf(&ggufs)?,
        };
        let name = entry.path.rsplit('/').next().unwrap_or(&entry.path);
        let dest = dest_dir.join(name);
        eprintln!(
            "downloading {} ({:.1} MB)",
            entry.path,
            mb(entry.byte_len())
        );
        download_file(
            &client,
            &RemoteFile {
                repo: &repo,
                revision: &revision,
                remote_path: &entry.path,
                token: token.as_deref(),
                expected_size: entry.byte_len(),
                expected_sha256: entry.sha256(),
            },
            &dest,
        )
        .await?;
        return Ok(dest);
    }

    if file.is_some() {
        bail!("--file is only valid for GGUF repos; {repo} is a safetensors snapshot");
    }

    let wanted: Vec<&TreeEntry> = files
        .iter()
        .copied()
        .filter(|e| snapshot_wanted(&e.path))
        .collect();
    if !wanted.iter().any(|e| e.path.ends_with(".safetensors")) {
        bail!(
            "{repo}@{revision} has no .safetensors weights and no .gguf files — unsupported repo \
             layout for the FORGE loader"
        );
    }
    if !wanted.iter().any(|e| e.path == "config.json") {
        bail!("{repo}@{revision} snapshot has no config.json — cannot be loaded by FORGE");
    }
    eprintln!("downloading snapshot: {} files", wanted.len());
    for entry in &wanted {
        let dest = dest_dir.join(&entry.path);
        eprintln!(
            "downloading {} ({:.1} MB)",
            entry.path,
            mb(entry.byte_len())
        );
        download_file(
            &client,
            &RemoteFile {
                repo: &repo,
                revision: &revision,
                remote_path: &entry.path,
                token: token.as_deref(),
                expected_size: entry.byte_len(),
                expected_sha256: entry.sha256(),
            },
            &dest,
        )
        .await?;
    }
    Ok(dest_dir)
}
