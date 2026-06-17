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
use std::time::Instant;

use base64::Engine;

use crate::ml_studio::train_recognition::{blob_content_hash, zip_dir};

/// Rozmiar fragmentu (600 KiB surowych bajtów → ~800 KiB base64, pod limitem ramki).
const ARTIFACT_CHUNK_BYTES: usize = 600 * 1024;
/// Twarde limity chroniące odbiorcę przed DoS (ogromny `total`, brak finalizacji).
const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_ARTIFACT_CHUNKS: u32 = 200_000;
const MAX_TOTAL_RECV_BYTES: u64 = 24 * 1024 * 1024 * 1024;
const RECV_TTL: std::time::Duration = std::time::Duration::from_secs(900);

struct RecvAccum {
    total: u32,
    chunks: Vec<Option<Vec<u8>>>,
    bytes: u64,
    last_touch: Instant,
}

/// Akumulator odbieranych fragmentów, kluczowany `(sender_node_id, transfer_id)`
/// — różni nadawcy nie kolidują nawet przy tym samym content-hashu.
static RECV: OnceLock<Mutex<HashMap<String, RecvAccum>>> = OnceLock::new();

fn recv_map() -> &'static Mutex<HashMap<String, RecvAccum>> {
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

/// Czy `path` jest dozwoloną lokalizacją artefaktu (fail-closed dla `MlArtifactPushTo`
/// od zdalnego węzła). Wymaga istniejącego KATALOGU pod znanym rootem ML Studio.
pub fn is_allowed_artifact_dir(path: &str) -> bool {
    let p = Path::new(path);
    let canon = match std::fs::canonicalize(p) {
        Ok(c) => c,
        Err(_) => return false,
    };
    if !canon.is_dir() {
        return false;
    }
    let roots = [
        crate::paths::cache_dir(),
        crate::paths::data_dir(),
        crate::paths::tentaflow_home().to_path_buf(),
    ];
    if roots.iter().any(|r| {
        std::fs::canonicalize(r)
            .map(|rc| canon.starts_with(&rc))
            .unwrap_or(false)
    }) {
        return true;
    }
    // Artefakty treningowe lądują w `ml-training-out/exports/...` (poza home przy
    // konfigurowalnym katalogu modeli) — dopuszczamy po komponencie ścieżki.
    let s = canon.to_string_lossy();
    s.contains("/ml-training-out/") || s.contains("/exports/") || s.contains("/mlx-model")
}

/// Węzeł-źródło: pakuje `src_dir` do ZIP i streamuje fragmenty do `target_node_id`
/// komendą `MlArtifactChunk`. Zwraca ścieżkę katalogu artefaktu NA węźle docelowym.
pub async fn push_dir_to(
    iroh: &crate::mesh::iroh_manager::IrohMeshManager,
    target_node_id: &str,
    src_dir: &str,
) -> anyhow::Result<String> {
    if !Path::new(src_dir).is_dir() {
        anyhow::bail!("artefakt do transferu nie jest katalogiem: {}", src_dir);
    }
    let zip = zip_dir(Path::new(src_dir))?;
    if zip.len() as u64 > MAX_ARTIFACT_BYTES {
        anyhow::bail!("artefakt przekracza limit transferu ({} B)", zip.len());
    }
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

/// Węzeł docelowy: przyjmuje jeden fragment od `sender_node_id`. Po komplecie
/// weryfikuje content-hash, składa ZIP i rozpakowuje do unikalnego katalogu
/// `<cache>/ml-studio/mesh-artifacts/<name>-<transfer_id>`. Zwraca `(true, ścieżka)`;
/// dla fragmentów pośrednich `(false, "")`.
pub fn recv_chunk(
    sender_node_id: &str,
    transfer_id: &str,
    name: &str,
    seq: u32,
    total: u32,
    data_b64: &str,
) -> anyhow::Result<(bool, String)> {
    if total == 0 || total > MAX_ARTIFACT_CHUNKS || seq >= total {
        anyhow::bail!("nieprawidłowy fragment artefaktu (seq={}, total={})", seq, total);
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64.as_bytes())
        .map_err(|e| anyhow::anyhow!("artifact chunk base64: {}", e))?;

    let key = format!("{}:{}", sender_node_id, transfer_id);
    let assembled = {
        let mut map = recv_map().lock().map_err(|_| anyhow::anyhow!("recv lock poisoned"))?;
        let now = Instant::now();
        // TTL sweep — porzucone transfery nie rosną w nieskończoność.
        map.retain(|_, a| now.duration_since(a.last_touch) < RECV_TTL);

        // Globalny limit pamięci wszystkich transferów w toku (poza tym fragmentem).
        let other: u64 = map.iter().filter(|(k, _)| *k != &key).map(|(_, a)| a.bytes).sum();
        if other + bytes.len() as u64 > MAX_TOTAL_RECV_BYTES {
            anyhow::bail!("bufor transferów artefaktów pełny — spróbuj później");
        }

        let entry = map.entry(key.clone()).or_insert_with(|| RecvAccum {
            total,
            chunks: (0..total).map(|_| None).collect(),
            bytes: 0,
            last_touch: now,
        });
        if entry.total != total || entry.chunks.len() != total as usize {
            map.remove(&key);
            anyhow::bail!("niespójny total dla transferu {}", transfer_id);
        }
        entry.last_touch = now;
        if entry.chunks[seq as usize].is_none() {
            entry.bytes += bytes.len() as u64;
            if entry.bytes > MAX_ARTIFACT_BYTES {
                map.remove(&key);
                anyhow::bail!("artefakt przekracza limit transferu");
            }
            entry.chunks[seq as usize] = Some(bytes);
        }
        if entry.chunks.iter().all(|c| c.is_some()) {
            let mut zip = Vec::with_capacity(entry.bytes as usize);
            for c in entry.chunks.iter() {
                zip.extend_from_slice(c.as_ref().unwrap());
            }
            map.remove(&key);
            Some(zip)
        } else {
            None
        }
    };

    let Some(zip) = assembled else {
        return Ok((false, String::new()));
    };

    // Integralność: transfer_id JEST content-hashem ZIP-a u nadawcy.
    if blob_content_hash(&zip) != transfer_id {
        anyhow::bail!("content-hash artefaktu nie zgadza się — transfer uszkodzony");
    }

    // Unikalny katalog docelowy (nazwa + transfer_id) — bez wyścigu z innym modelem.
    let dest = artifact_dest_root().join(format!("{}-{}", safe_artifact_name(name), transfer_id));
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
