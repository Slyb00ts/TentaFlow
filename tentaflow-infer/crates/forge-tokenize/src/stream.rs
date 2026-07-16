// ===== File: stream.rs — incremental UTF-8-safe streaming detokenizer =====
//
// Byte-fallback BPE (and byte-level BPE in general) can split a single UTF-8
// scalar across several tokens, so decoding token-by-token would emit U+FFFD
// replacement characters mid-stream. This decoder uses a sliding two-offset
// window (the scheme TGI/vLLM use): decode the window with and without the
// newest tokens and only emit the diff once it no longer ends in an
// incomplete sequence. Decoding a window (not single ids) also keeps
// context-sensitive decoders correct (SPM `▁` strip, Fuse, ByteFallback).

use std::collections::HashMap;
use std::sync::OnceLock;

use forge_types::Result;

use crate::tokenizer::Tokenizer;

/// How raw bytes are reconstructed from vocab pieces, to tell a genuinely
/// generated U+FFFD apart from an incomplete multi-byte tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawByteMode {
    /// GPT-2 byte-level alphabet: every piece char maps back to one byte.
    ByteLevel,
    /// SPM byte fallback: `<0xXX>` pieces are single bytes, other pieces are
    /// text with `▁` for spaces.
    ByteFallback,
    /// Unrecognized decoder: raw bytes unavailable, hold back conservatively.
    Unknown,
}

pub struct StreamDecoder<'t> {
    tokenizer: &'t Tokenizer,
    skip_special_tokens: bool,
    raw_mode: RawByteMode,
    ids: Vec<u32>,
    // Window start: tokens before this offset are already fully emitted and
    // no longer participate in decoding.
    prefix_offset: usize,
    // Tokens in [prefix_offset, read_offset) produced the text emitted so far.
    read_offset: usize,
}

impl<'t> StreamDecoder<'t> {
    pub fn new(tokenizer: &'t Tokenizer, skip_special_tokens: bool) -> Self {
        Self {
            tokenizer,
            skip_special_tokens,
            raw_mode: detect_raw_byte_mode(tokenizer),
            ids: Vec::new(),
            prefix_offset: 0,
            read_offset: 0,
        }
    }

    /// Feed one token id; returns the complete UTF-8 text that became
    /// unambiguous with this token (possibly empty while bytes accumulate).
    pub fn push(&mut self, id: u32) -> Result<String> {
        self.ids.push(id);
        let prev = self.decode_window(self.read_offset)?;
        let full = self.decode_window(self.ids.len())?;
        // A trailing U+FFFD can be either an incomplete multi-byte tail (hold
        // until more bytes arrive) or text the model genuinely produced (the
        // literal replacement char, or invalid bytes that no continuation can
        // fix) — the raw pending bytes decide which.
        let hold_for_more_bytes = full.ends_with('\u{FFFD}') && self.window_tail_incomplete();
        if full.len() > prev.len() && !hold_for_more_bytes && full.starts_with(&prev) {
            let emitted = full[prev.len()..].to_string();
            self.prefix_offset = self.read_offset;
            self.read_offset = self.ids.len();
            return Ok(emitted);
        }
        Ok(String::new())
    }

    /// Flush any held-back text at end of stream. Truncated byte sequences
    /// (model stopped mid-scalar) surface as U+FFFD here rather than being
    /// silently dropped.
    pub fn finish(&mut self) -> Result<String> {
        let prev = self.decode_window(self.read_offset)?;
        let full = self.decode_window(self.ids.len())?;
        self.prefix_offset = self.ids.len();
        self.read_offset = self.ids.len();
        if full.starts_with(&prev) {
            Ok(full[prev.len()..].to_string())
        } else {
            // Decoder changed earlier text retroactively (pathological
            // normalizer interaction); emit the full window rather than lose it.
            Ok(full)
        }
    }

    fn decode_window(&self, end: usize) -> Result<String> {
        self.tokenizer
            .decode(&self.ids[self.prefix_offset..end], self.skip_special_tokens)
    }

    /// True when the raw bytes of the current window end in a valid but
    /// incomplete UTF-8 sequence that further tokens could still complete.
    /// Genuine U+FFFD bytes (0xEF 0xBF 0xBD) are complete UTF-8 and yield
    /// false. Falls back to true (hold) when raw bytes cannot be recovered.
    fn window_tail_incomplete(&self) -> bool {
        match self.window_raw_bytes() {
            Some(bytes) => ends_with_incomplete_utf8(&bytes),
            None => true,
        }
    }

    /// Reconstruct the raw byte stream of the current decode window from the
    /// vocab pieces. Cosmetic decoder steps (`▁` strip, Fuse) are irrelevant
    /// here: only whether the trailing bytes form an incomplete scalar matters.
    fn window_raw_bytes(&self) -> Option<Vec<u8>> {
        if self.raw_mode == RawByteMode::Unknown {
            return None;
        }
        let added = self.tokenizer.inner().get_added_vocabulary();
        let mut bytes = Vec::new();
        for &id in &self.ids[self.prefix_offset..] {
            let piece = self.tokenizer.token_to_piece(id)?;
            if self.skip_special_tokens && added.is_special_token(&piece) {
                continue;
            }
            match self.raw_mode {
                RawByteMode::ByteLevel => {
                    let mut utf8 = [0u8; 4];
                    for c in piece.chars() {
                        match byte_level_char_to_byte(c) {
                            Some(b) => bytes.push(b),
                            // Added tokens (chat markers etc.) are stored as
                            // plain text, not in the byte-level alphabet.
                            None => bytes.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes()),
                        }
                    }
                }
                RawByteMode::ByteFallback => match parse_byte_fallback_piece(&piece) {
                    Some(b) => bytes.push(b),
                    None => bytes.extend_from_slice(piece.replace('▁', " ").as_bytes()),
                },
                RawByteMode::Unknown => unreachable!(),
            }
        }
        Some(bytes)
    }
}

/// True when the byte stream ends in an incomplete (but so far valid) UTF-8
/// sequence. Only the trailing state matters: earlier invalid bytes are
/// skipped like `from_utf8_lossy` does.
fn ends_with_incomplete_utf8(mut bytes: &[u8]) -> bool {
    loop {
        match std::str::from_utf8(bytes) {
            Ok(_) => return false,
            Err(e) => match e.error_len() {
                Some(n) => bytes = &bytes[e.valid_up_to() + n..],
                None => return true,
            },
        }
    }
}

/// Classify the tokenizer's decoder to pick the piece→bytes strategy. The
/// decoder wrappers keep their internals private, so the serialized form
/// (stable `"type"` tags) is inspected instead.
fn detect_raw_byte_mode(tokenizer: &Tokenizer) -> RawByteMode {
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

/// SPM byte-fallback piece `<0xXX>` → byte value.
fn parse_byte_fallback_piece(piece: &str) -> Option<u8> {
    let hex = piece.strip_prefix("<0x")?.strip_suffix('>')?;
    if hex.len() != 2 {
        return None;
    }
    u8::from_str_radix(hex, 16).ok()
}

/// Inverse of the GPT-2 byte→unicode alphabet: printable latin bytes map to
/// themselves, the remaining 68 bytes map to U+0100.. in byte order.
fn byte_level_char_to_byte(c: char) -> Option<u8> {
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
