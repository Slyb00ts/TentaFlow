// ===== File: rawbytes.rs — reconstruct a token's raw output bytes from its vocab piece =====
// Shared by the streaming detokenizer (incomplete-UTF-8 detection) and the
// grammar byte table (constrained decoding). A token's raw bytes are what it
// contributes to the output stream, recovered from the vocab piece according
// to the tokenizer's decoder family (GPT-2 byte-level alphabet or SPM byte
// fallback).

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::tokenizer::Tokenizer;

/// How raw bytes are reconstructed from vocab pieces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawByteMode {
    /// GPT-2 byte-level alphabet: every piece char maps back to one byte.
    ByteLevel,
    /// SPM byte fallback: `<0xXX>` pieces are single bytes, other pieces are
    /// text with `▁` for spaces.
    ByteFallback,
    /// Unrecognized decoder: raw bytes unavailable.
    Unknown,
}

/// Classify the tokenizer's decoder to pick the piece→bytes strategy. The
/// decoder wrappers keep their internals private, so the serialized form
/// (stable `"type"` tags) is inspected instead.
pub fn detect_raw_byte_mode(tokenizer: &Tokenizer) -> RawByteMode {
    let Some(decoder) = tokenizer.inner().get_decoder() else {
        return RawByteMode::Unknown;
    };
    let Ok(json) = serde_json::to_string(decoder) else {
        return RawByteMode::Unknown;
    };
    if json.contains("\"type\":\"ByteLevel\"") {
        RawByteMode::ByteLevel
    } else if json.contains("\"type\":\"ByteFallback\"") {
        RawByteMode::ByteFallback
    } else {
        RawByteMode::Unknown
    }
}

/// Reconstruct one piece's raw output bytes under `mode`. Returns `None` when
/// bytes cannot be recovered (unknown decoder).
pub fn piece_raw_bytes(mode: RawByteMode, piece: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    match mode {
        RawByteMode::ByteLevel => {
            let mut utf8 = [0u8; 4];
            for c in piece.chars() {
                match byte_level_char_to_byte(c) {
                    Some(b) => bytes.push(b),
                    // Added tokens (chat markers etc.) are stored as plain
                    // text, not in the byte-level alphabet.
                    None => bytes.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes()),
                }
            }
            Some(bytes)
        }
        RawByteMode::ByteFallback => match parse_byte_fallback_piece(piece) {
            Some(b) => Some(vec![b]),
            None => Some(piece.replace('▁', " ").into_bytes()),
        },
        RawByteMode::Unknown => None,
    }
}

/// SPM byte-fallback piece `<0xXX>` → byte value.
pub fn parse_byte_fallback_piece(piece: &str) -> Option<u8> {
    let hex = piece.strip_prefix("<0x")?.strip_suffix('>')?;
    if hex.len() != 2 {
        return None;
    }
    u8::from_str_radix(hex, 16).ok()
}

/// Inverse of the GPT-2 byte→unicode alphabet: printable latin bytes map to
/// themselves, the remaining 68 bytes map to U+0100.. in byte order.
pub fn byte_level_char_to_byte(c: char) -> Option<u8> {
    static TABLE: OnceLock<HashMap<char, u8>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let printable = |b: u32| {
            (0x21..=0x7E).contains(&b) || (0xA1..=0xAC).contains(&b) || (0xAE..=0xFF).contains(&b)
        };
        let mut map = HashMap::with_capacity(256);
        let mut n = 0u32;
        for b in 0u32..256 {
            let c = if printable(b) {
                char::from_u32(b).unwrap()
            } else {
                let c = char::from_u32(256 + n).unwrap();
                n += 1;
                c
            };
            map.insert(c, b as u8);
        }
        map
    });
    table.get(&c).copied()
}
