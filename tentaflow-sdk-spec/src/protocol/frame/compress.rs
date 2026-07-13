// =============================================================================
// File: protocol/frame/compress.rs — lz4 frame compression (UFP/2 §8)
// Purpose: compress and decompress envelope bodies using the lz4 FRAME format
// (RFC-style header + blocks + content checksum), not raw lz4 blocks. The
// frame format is self-describing so any UFP/2 port (Rust, Zig, Go, C#,
// Python) can interop using its language's standard lz4 library.
//
// Threshold: senders SHOULD only set IS_COMPRESSED when body > 4096 bytes
// (helper `should_compress`). Receivers ALWAYS honour IS_COMPRESSED if set,
// regardless of size — a malicious sender setting IS_COMPRESSED on a tiny
// payload is decoded by lz4_flex and either succeeds or fails cleanly.
//
// CRIME/BREACH side channel (§8.1) is a per-channel policy concern enforced
// in the 4c1g structural validator, not here.
//
// Spec ref: docs/UNIFIED_FRAME_PROTOCOL_v2.md §8 + §8.1 + §7.1 pipeline.
// =============================================================================

use lz4_flex::frame::{FrameDecoder, FrameEncoder, FrameInfo};
use std::io::{Read, Write};

use super::error::{FrameError, FrameErrorCode};

/// Suggested minimum body size before a sender SHOULD switch on
/// `IS_COMPRESSED`. Smaller payloads pay more in frame overhead than they
/// save in bytes (§8).
pub const COMPRESSION_THRESHOLD_BYTES: usize = 4096;

/// Default zip-bomb cap: receivers MUST refuse to decompress past 64 MB of
/// plaintext output unless they explicitly raise the cap for a specific
/// channel (matches §10.3 reassembly buffer ceiling). Without a cap, a
/// small adversary-crafted lz4 frame can claim gigabytes of memory.
pub const DEFAULT_MAX_DECOMPRESSED_BYTES: usize = 64 * 1024 * 1024;

/// Return `true` when a sender SHOULD set `IS_COMPRESSED` and run
/// `compress_body` on a body of the given length. The §8 threshold is a
/// guideline, not a hard rule — callers MAY force compression below the
/// threshold for testing or override it for specific channels.
pub fn should_compress(body_len: usize) -> bool {
    body_len > COMPRESSION_THRESHOLD_BYTES
}

/// lz4-frame-compress a body with content checksum enabled. Returns the
/// compressed bytes ready to replace `envelope.body` (before AEAD encryption,
/// per §7.1 pipeline order). Content checksum adds 4 bytes of overhead to
/// the trailing footer but lets receivers catch corruption of compressed
/// bytes when AEAD is NOT in use (e.g. plaintext channels under TLS).
pub fn compress_body(plaintext: &[u8]) -> Result<Vec<u8>, FrameError> {
    let info = FrameInfo::new().content_checksum(true);
    let mut encoder =
        FrameEncoder::with_frame_info(info, Vec::with_capacity(plaintext.len() / 2 + 64));
    encoder.write_all(plaintext).map_err(|e| {
        FrameError::new(
            FrameErrorCode::DecompressionFailed,
            format!("compress_body: lz4 frame write failed: {e}"),
        )
    })?;
    let out = encoder.finish().map_err(|e| {
        FrameError::new(
            FrameErrorCode::DecompressionFailed,
            format!("compress_body: lz4 frame finish failed: {e}"),
        )
    })?;
    Ok(out)
}

/// lz4-frame-decompress a body. Returns the recovered plaintext bytes.
/// Caps decompressed output at `DEFAULT_MAX_DECOMPRESSED_BYTES` (64 MB) —
/// callers who need a different cap MUST use `decompress_body_with_limit`.
pub fn decompress_body(compressed: &[u8]) -> Result<Vec<u8>, FrameError> {
    decompress_body_with_limit(compressed, DEFAULT_MAX_DECOMPRESSED_BYTES)
}

/// lz4-frame-decompress with an explicit plaintext byte cap. Returns
/// `DecompressionFailed` (§11 code 0x000B) on any lz4 error OR when the
/// output would exceed `max_output_bytes`. This is the anti-zip-bomb gate;
/// every production receive path MUST go through this function (directly
/// or via `decompress_body`).
pub fn decompress_body_with_limit(
    compressed: &[u8],
    max_output_bytes: usize,
) -> Result<Vec<u8>, FrameError> {
    let decoder = FrameDecoder::new(compressed);
    // `take` enforces the hard byte ceiling at the Read layer: lz4_flex will
    // signal end-of-stream once the limit is reached. We then verify that
    // the underlying decoder actually finished, not that we hit the cap.
    let mut limited = decoder.take(max_output_bytes as u64 + 1);
    // Cap prealloc by the output limit so a small adversarial input cannot
    // induce an oversize Vec allocation even if it never actually decompresses.
    let initial_cap = compressed
        .len()
        .saturating_mul(2)
        .min(max_output_bytes.saturating_add(1));
    let mut plaintext = Vec::with_capacity(initial_cap);
    limited.read_to_end(&mut plaintext).map_err(|e| {
        FrameError::new(
            FrameErrorCode::DecompressionFailed,
            format!("decompress_body: lz4 frame read failed: {e}"),
        )
    })?;
    if plaintext.len() > max_output_bytes {
        return Err(FrameError::new(
            FrameErrorCode::DecompressionFailed,
            format!(
                "decompress_body: plaintext exceeded max_output_bytes={} (zip-bomb guard)",
                max_output_bytes
            ),
        ));
    }
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_decompress_roundtrip_text() {
        let plaintext = b"the quick brown fox jumps over the lazy dog ".repeat(64);
        let compressed = compress_body(&plaintext).unwrap();
        let recovered = decompress_body(&compressed).unwrap();
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn compress_decompress_roundtrip_random_bytes() {
        let mut plaintext = Vec::with_capacity(8192);
        for i in 0..8192u32 {
            plaintext.push((i.wrapping_mul(2654435761) & 0xFF) as u8);
        }
        let compressed = compress_body(&plaintext).unwrap();
        let recovered = decompress_body(&compressed).unwrap();
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn compress_decompress_empty_body() {
        let plaintext: Vec<u8> = Vec::new();
        let compressed = compress_body(&plaintext).unwrap();
        let recovered = decompress_body(&compressed).unwrap();
        assert!(recovered.is_empty());
    }

    #[test]
    fn compressed_repetitive_payload_is_smaller_than_input() {
        let plaintext = vec![0xABu8; 16 * 1024];
        let compressed = compress_body(&plaintext).unwrap();
        assert!(
            compressed.len() < plaintext.len() / 4,
            "highly compressible payload should shrink at least 4x; got {} vs {}",
            compressed.len(),
            plaintext.len()
        );
    }

    #[test]
    fn decompress_rejects_garbage() {
        let r = decompress_body(b"not actually lz4 frame bytes at all");
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::DecompressionFailed);
    }

    #[test]
    fn decompress_rejects_truncated_frame() {
        let plaintext = b"some bytes worth compressing".repeat(32);
        let compressed = compress_body(&plaintext).unwrap();
        let truncated = &compressed[..compressed.len() / 2];
        let r = decompress_body(truncated);
        assert!(r.is_err());
    }

    #[test]
    fn should_compress_threshold_matches_spec() {
        assert!(!should_compress(0));
        assert!(!should_compress(4096));
        assert!(should_compress(4097));
        assert!(should_compress(1024 * 1024));
    }

    #[test]
    fn decompress_rejects_content_checksum_mismatch() {
        let plaintext = b"deterministic test payload for corruption detection ".repeat(64);
        let mut compressed = compress_body(&plaintext).unwrap();
        // Flip a byte that lands inside the compressed block body, not the
        // frame header. With content_checksum=true the trailing checksum
        // catches the corruption.
        let mid = compressed.len() / 2;
        compressed[mid] ^= 0xFF;
        let r = decompress_body(&compressed);
        assert!(
            r.is_err(),
            "lz4 frame with content_checksum=true should detect mid-stream tamper"
        );
    }

    #[test]
    fn decompress_with_limit_caps_output() {
        let plaintext = vec![0u8; 128 * 1024];
        let compressed = compress_body(&plaintext).unwrap();
        let r = decompress_body_with_limit(&compressed, 64 * 1024);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::DecompressionFailed);
    }

    #[test]
    fn decompress_with_limit_passes_under_cap() {
        let plaintext = b"abcdef".repeat(1024); // 6 KiB
        let compressed = compress_body(&plaintext).unwrap();
        let recovered = decompress_body_with_limit(&compressed, 64 * 1024).unwrap();
        assert_eq!(recovered, plaintext);
    }
}
