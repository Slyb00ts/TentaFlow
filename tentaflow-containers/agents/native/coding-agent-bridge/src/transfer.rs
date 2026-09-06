// ============ File: transfer.rs — idle-account transfer barriers behind authenticated IPC ============
use crate::{credential_name, ensure_idle_runtime, ApiError, AppState, Provider};
use anyhow::{anyhow, Context, Result};
use axum::{extract::State, Json};
use serde_json::{json, Value};
use std::{io::Write, path::Path};

pub fn available(state: &AppState) -> Result<()> {
    if state.shutting_down.load(std::sync::atomic::Ordering::SeqCst) { return Err(anyhow!("account runtime is stopping")); }
    if state
        .state_file
        .parent()
        .context("account root missing")?
        .join("transfer.json")
        .exists()
    {
        return Err(anyhow!(
            "account_moving: this account is frozen for relocation"
        ));
    }
    Ok(())
}

pub fn write_private(path: &Path, value: &Value) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(&temporary)?;
    output.write_all(&serde_json::to_vec(value)?)?;
    output.sync_all()?;
    std::fs::rename(temporary, path)?;
    std::fs::File::open(path.parent().context("state directory missing")?)?.sync_all()?;
    Ok(())
}

fn identifier(value: &Value) -> Result<&str> {
    let id = value
        .get("transfer_id")
        .and_then(Value::as_str)
        .context("transfer_id is required")?;
    if uuid::Uuid::parse_str(id)?.to_string() != id {
        return Err(anyhow!("invalid transfer identifier"));
    }
    Ok(id)
}

pub async fn freeze(
    State(state): State<AppState>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let _lease = state.lease.lock().await;
    if _lease.is_some() {
        return Err(ApiError::bad_request(
            "account_busy: finish the active session before moving this account",
        ));
    }
    ensure_idle_runtime(&state)?;
    if !matches!(state.provider, Provider::Codex | Provider::ClaudeCode) {
        return Err(ApiError::bad_request(
            "provider credential portability is not verified",
        ));
    }
    let id = identifier(&request)?;
    let manifest = request
        .get("manifest")
        .context("transfer manifest missing")?;
    let root = state.state_file.parent().context("account root missing")?;
    if root.join("credential-review-required").exists() {
        return Err(ApiError::bad_request(
            "sign in again before moving credentials awaiting verification",
        ));
    }
    let marker = root.join("transfer.json");
    let expected = json!({"transfer_id":id,"phase":"source_frozen","manifest":manifest});
    if marker.exists() {
        let existing: Value =
            serde_json::from_slice(&std::fs::read(&marker)?).context("invalid transfer state")?;
        if existing != expected {
            return Err(ApiError::bad_request(
                "account has a different transfer in progress",
            ));
        }
    }
    let (directory, file) = credential_name(state.provider);
    let path = root.join(directory).join(file);
    if path
        .parent()
        .context("credential directory missing")?
        .canonicalize()?
        != root.canonicalize()?.join(directory)
    {
        return Err(ApiError::bad_request(
            "portable credential directory is redirected",
        ));
    }
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024 {
        return Err(ApiError::bad_request("invalid portable credential file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(ApiError::bad_request("portable credential has hardlinks"));
        }
    }
    let credential: Value =
        serde_json::from_slice(&std::fs::read(path)?).context("invalid portable credential")?;
    if !credential
        .as_object()
        .is_some_and(|object| !object.is_empty())
    {
        return Err(ApiError::bad_request("empty portable credential"));
    }
    write_private(&marker, &expected)?;
    Ok(Json(json!({"credential":credential})))
}

pub async fn retire(
    State(state): State<AppState>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let _lease = state.lease.lock().await;
    if _lease.is_some() {
        return Err(ApiError::bad_request("account is busy"));
    }
    let id = identifier(&request)?;
    let path = state
        .state_file
        .parent()
        .context("account root missing")?
        .join("transfer.json");
    let mut marker: Value =
        serde_json::from_slice(&std::fs::read(&path)?).context("invalid transfer state")?;
    if marker["transfer_id"] != id
        || !matches!(
            marker["phase"].as_str(),
            Some("source_frozen" | "source_retired")
        )
    {
        return Err(ApiError::bad_request("transfer state mismatch"));
    }
    marker["phase"] = json!("source_retired");
    write_private(&path, &marker)?;
    Ok(Json(json!({"retired":true})))
}

pub async fn activate(
    State(state): State<AppState>,
    Json(request): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let _lease = state.lease.lock().await;
    if _lease.is_some() {
        return Err(ApiError::bad_request("account is busy"));
    }
    let id = identifier(&request)?;
    let path = state
        .state_file
        .parent()
        .context("account root missing")?
        .join("transfer.json");
    if path.exists() {
        let marker: Value =
            serde_json::from_slice(&std::fs::read(&path)?).context("invalid transfer state")?;
        if marker["transfer_id"] != id || marker["phase"] != "target_staged" {
            return Err(ApiError::bad_request("transfer state mismatch"));
        }
        std::fs::remove_file(&path)?;
        std::fs::File::open(path.parent().context("account root missing")?)?.sync_all()?;
    }
    Ok(Json(json!({"activated":true})))
}
