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

use crate::tokenizer::Tokenizer;

pub struct StreamDecoder<'t> {
    tokenizer: &'t Tokenizer,
    skip_special_tokens: bool,
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
        // A trailing U+FFFD means the newest token ended mid-scalar (byte
        // fallback); hold everything until the sequence completes.
        if full.len() > prev.len() && !full.ends_with('\u{FFFD}') && full.starts_with(&prev) {
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
}
