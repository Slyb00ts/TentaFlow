// =============================================================================
// File: services/recording/snapshot.rs — PNG snapshot encoder + writer
// =============================================================================
//
// Encodes RGB24 buffers to PNG via the `image` crate, hashes the bytes, and
// writes them atomically to the per-camera snapshot directory. The encode is
// CPU-bound and tiny for typical 1080p frames (~30 ms); we run it inside
// `spawn_blocking` to keep the async runtime healthy under bursts.

use std::path::PathBuf;

use image::{ImageFormat, RgbImage};

use super::error::{RecordingError, Result};
use super::storage::{
    atomic_write, camera_subdir, recording_base_dir, sha256_hex, validate_camera_id, RecordingKind,
    RecordingRef,
};
use super::SavedRecording;

/// Encode + persist an RGB24 frame as PNG. Returns the catalog entry the
/// caller will insert into the `recordings` table (DB write is Chunk C).
pub async fn save_snapshot_rgb24(
    camera_id: &str,
    rgb24_data: &[u8],
    width: u32,
    height: u32,
) -> Result<SavedRecording> {
    validate_camera_id(camera_id)?;
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(3))
        .ok_or_else(|| RecordingError::InvalidDimensions(rgb24_data.len(), width, height))?;
    if rgb24_data.len() != expected {
        return Err(RecordingError::InvalidDimensions(
            rgb24_data.len(),
            width,
            height,
        ));
    }

    let recording_ref = RecordingRef(format!("snap_{}", uuid::Uuid::new_v4()));
    let base = recording_base_dir()?;
    let dir = camera_subdir(&base, camera_id, RecordingKind::Snapshot);
    tokio::fs::create_dir_all(&dir).await?;
    let file_path: PathBuf = dir.join(format!("{}.png", recording_ref.0));

    let owned = rgb24_data.to_vec();
    let png_bytes = tokio::task::spawn_blocking(move || encode_png_sync(owned, width, height))
        .await
        .map_err(|e| RecordingError::PngEncode(format!("join: {e}")))??;

    let hash = sha256_hex(&png_bytes);
    let file_size_bytes = png_bytes.len() as u64;
    atomic_write(&file_path, &png_bytes).await?;

    Ok(SavedRecording {
        recording_ref,
        kind: RecordingKind::Snapshot,
        file_path,
        file_size_bytes,
        duration_ms: None,
        width: Some(width),
        height: Some(height),
        pixel_format: Some("png".into()),
        hash_sha256: hash,
        created_at: now_unix_secs(),
    })
}

fn encode_png_sync(rgb24: Vec<u8>, width: u32, height: u32) -> Result<Vec<u8>> {
    let img = RgbImage::from_raw(width, height, rgb24).ok_or_else(|| {
        RecordingError::PngEncode("invalid dimensions vs buffer size".into())
    })?;
    let mut buf = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut buf);
    img.write_to(&mut cursor, ImageFormat::Png)
        .map_err(|e| RecordingError::PngEncode(e.to_string()))?;
    Ok(buf)
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb_buf(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                v.push((x % 256) as u8);
                v.push((y % 256) as u8);
                v.push(((x + y) % 256) as u8);
            }
        }
        v
    }

    fn temp_home_guard() -> (
        std::sync::MutexGuard<'static, ()>,
        tempfile::TempDir,
    ) {
        let guard = super::super::storage::home_sandbox_lock();
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", d.path());
        (guard, d)
    }

    #[tokio::test]
    async fn test_save_snapshot_basic() {
        let _home = temp_home_guard();
        let rgb = rgb_buf(64, 48);
        let saved = save_snapshot_rgb24("cam_unit", &rgb, 64, 48).await.expect("save ok");
        assert!(saved.file_path.exists());
        assert!(saved.file_size_bytes > 0);
        assert_eq!(saved.hash_sha256.len(), 64);
        assert_eq!(saved.width, Some(64));
        assert_eq!(saved.height, Some(48));
        assert_eq!(saved.pixel_format.as_deref(), Some("png"));
        assert!(saved.recording_ref.as_str().starts_with("snap_"));
        // Hash matches a fresh read of the file (no partial write).
        let on_disk = tokio::fs::read(&saved.file_path).await.unwrap();
        assert_eq!(sha256_hex_local(&on_disk), saved.hash_sha256);
    }

    fn sha256_hex_local(b: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(b);
        format!("{:x}", h.finalize())
    }

    #[tokio::test]
    async fn test_save_snapshot_invalid_dimensions() {
        let _home = temp_home_guard();
        let rgb = vec![0u8; 100]; // not 64*48*3
        let err = save_snapshot_rgb24("cam_unit", &rgb, 64, 48).await.unwrap_err();
        assert!(matches!(err, RecordingError::InvalidDimensions(100, 64, 48)));
    }

    #[tokio::test]
    async fn test_save_snapshot_rejects_bad_camera_id() {
        let _home = temp_home_guard();
        let rgb = rgb_buf(4, 4);
        let err = save_snapshot_rgb24("../escape", &rgb, 4, 4).await.unwrap_err();
        assert!(matches!(err, RecordingError::InvalidCameraId));
    }

    #[tokio::test]
    async fn test_save_snapshot_atomic_no_tmp_leftover() {
        let _home = temp_home_guard();
        let rgb = rgb_buf(8, 8);
        let saved = save_snapshot_rgb24("cam_atomic", &rgb, 8, 8).await.unwrap();
        let tmp = {
            let mut s = saved.file_path.as_os_str().to_owned();
            s.push(".tmp");
            std::path::PathBuf::from(s)
        };
        assert!(!tmp.exists(), "tmp file must be renamed away on success");
    }
}
