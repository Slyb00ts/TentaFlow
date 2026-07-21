// ===== File: stream.rs — incremental UTF-8-safe streaming detokenizer =====
//
// Byte-fallback BPE (and byte-level BPE in general) can split a single UTF-8
// scalar across several tokens, so decoding token-by-token would emit U+FFFD
// replacement characters mid-stream. This decoder uses a sliding two-offset
// window (the scheme TGI/vLLM use): decode the window with and without the
// newest tokens and only emit the diff once it no longer ends in an
// incomplete sequence. Decoding a window (not single ids) also keeps
// context-sensitive decoders correct (SPM `▁` strip, Fuse, ByteFallback).

use forge_types::Result;

use crate::rawbytes::{piece_raw_bytes, RawByteMode};
use crate::tokenizer::Tokenizer;

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
            raw_mode: crate::rawbytes::detect_raw_byte_mode(tokenizer),
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
            bytes.extend(piece_raw_bytes(self.raw_mode, &piece)?);
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

