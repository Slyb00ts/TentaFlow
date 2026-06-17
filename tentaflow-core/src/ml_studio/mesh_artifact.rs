// ===== File: ml_studio/mesh_artifact.rs — transfer artefaktu modelu między węzłami mesh =====
//
// Przenosi katalog artefaktu modelu (np. model MLX safetensors) z węzła, na
// którym powstał (np. B), na węzeł docelowy deployu (np. C). Węzeł-źródło pakuje
// katalog do ZIP (Stored), tnie na fragmenty i streamuje je komendą mesh
// `MlArtifactChunk` do węzła docelowego, który składa i rozpakowuje do lokalnego
// katalogu. Pełny mesh (B↔C sparowane) → transfer idzie wprost B→C.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use base64::Engine;

use crate::ml_studio::train_recognition::{blob_content_hash, zip_dir};

/// Rozmiar fragmentu (600 KiB surowych bajtów → ~800 KiB base64, pod limitem ramki).
const ARTIFACT_CHUNK_BYTES: usize = 600 * 1024;

/// Akumulator odbieranych fragmentów: transfer_id → fragmenty wg seq.
static RECV: OnceLock<Mutex<HashMap<String, Vec<Option<Vec<u8>>>>>> = OnceLock::new();

fn recv_map() -> &'static Mutex<HashMap<String, Vec<Option<Vec<u8>>>>> {
    RECV.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Katalog, do którego węzeł docelowy rozpakowuje odebrane artefakty.
fn artifact_dest_root() -> PathBuf {
    crate::paths::cache_dir().join("ml-studio").join("mesh-artifacts")
}

/// Nazwa katalogu artefaktu na węźle docelowym (ostatni segment ścieżki źródła,
/// odcięty z path-traversal). Pusta/niebezpieczna → `model`.
fn safe_artifact_name(src_dir: &str) -> String {
    Path::new(src_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.replace(['/', '\\', '.'], "_"))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "model".to_string())
}

/// Węzeł-źródło: pakuje `src_dir` do ZIP i streamuje fragmenty do `target_node_id`
/// komendą `MlArtifactChunk`. Zwraca ścieżkę katalogu artefaktu NA węźle docelowym.
pub async fn push_dir_to(
    iroh: &crate::mesh::iroh_manager::IrohMeshManager,
    target_node_id: &str,
    src_dir: &str,
) -> anyhow::Result<String> {
    let zip = zip_dir(Path::new(src_dir))?;
    let transfer_id = blob_content_hash(&zip);
    let name = safe_artifact_name(src_dir);
    let total = zip.len().div_ceil(ARTIFACT_CHUNK_BYTES).max(1) as u32;

    let mut target_path = String::new();
    for seq in 0..total {
        let start = seq as usize * ARTIFACT_CHUNK_BYTES;
        let end = (start + ARTIFACT_CHUNK_BYTES).min(zip.len());
        let data_b64 = base64::engine::general_purpose::STANDARD.encode(&zip[start..end]);
        let cmd = tentaflow_protocol::mesh::MeshCommandType::MlArtifactChunk {
            transfer_id: transfer_id.clone(),
            name: name.clone(),
            seq,
            total,
            data_b64,
        };
        let resp = iroh
            .send_command_and_wait(target_node_id, cmd, 180)
            .await
            .map_err(|e| anyhow::anyhow!("transfer artefaktu (chunk {}): {}", seq, e))?;
        match resp.payload {
            tentaflow_protocol::mesh::MeshCommandResponsePayload::MlArtifactChunkResult {
                local_path,
                error,
            } => {
                if let Some(err) = error {
                    anyhow::bail!("węzeł docelowy odrzucił artefakt: {}", err);
                }
                if !local_path.is_empty() {
                    target_path = local_path;
                }
            }
            _ if !resp.ok => {
                anyhow::bail!(resp.error.unwrap_or_else(|| "transfer artefaktu nieudany".into()))
            }
            _ => {}
        }
    }

    if target_path.is_empty() {
        anyhow::bail!("węzeł docelowy nie zwrócił ścieżki artefaktu po transferze");
    }
    Ok(target_path)
}

/// Węzeł docelowy: przyjmuje jeden fragment. Po komplecie składa ZIP, rozpakowuje
/// do `<cache>/ml-studio/mesh-artifacts/<name>` i zwraca `(true, ścieżka)`.
/// Dla fragmentów pośrednich zwraca `(false, "")`.
pub fn recv_chunk(
    transfer_id: &str,
    name: &str,
    seq: u32,
    total: u32,
    data_b64: &str,
) -> anyhow::Result<(bool, String)> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.as_bytes())
        .map_err(|e| anyhow::anyhow!("artifact chunk base64: {}", e))?;

    let assembled = {
        let mut map = recv_map().lock().map_err(|_| anyhow::anyhow!("recv lock poisoned"))?;
        let slot = map.entry(transfer_id.to_string()).or_insert_with(|| vec![None; total as usize]);
        if slot.len() != total as usize {
            *slot = vec![None; total as usize];
        }
        if (seq as usize) < slot.len() {
            slot[seq as usize] = Some(bytes);
        }
        if slot.iter().all(|c| c.is_some()) {
            let mut zip = Vec::new();
            for c in slot.iter() {
                zip.extend_from_slice(c.as_ref().unwrap());
            }
            map.remove(transfer_id);
            Some(zip)
        } else {
            None
        }
    };

    let Some(zip) = assembled else {
        return Ok((false, String::new()));
    };

    let dest = artifact_dest_root().join(safe_artifact_name(name));
    unzip_to_dir(&zip, &dest)?;
    Ok((true, dest.to_string_lossy().to_string()))
}

/// Rozpakowuje ZIP do `dest` (czyści katalog najpierw). Odcina path-traversal.
fn unzip_to_dir(zip_bytes: &[u8], dest: &Path) -> anyhow::Result<()> {
    if dest.exists() {
        let _ = std::fs::remove_dir_all(dest);
    }
    std::fs::create_dir_all(dest)?;
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes))
        .map_err(|e| anyhow::anyhow!("artefakt nie jest poprawnym zip: {}", e))?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out_path = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf)?;
        std::fs::write(&out_path, &buf)?;
    }
    Ok(())
}
