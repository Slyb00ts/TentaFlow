// ===== File: events/step_io.rs — what a flow step received and produced, stored compressed =====
//
// A step row says WHEN a node ran; this says WHAT it was given and what it
// returned (prompts sent to a model, the model's answer, the passages a
// retrieval picked). Auditing an AI run needs the content, and prompts repeat
// the same context over and over, so the pair is zstd-compressed before it is
// written: text of this kind shrinks several times over.
//
// Each side is capped so one oversized payload cannot dominate the file; the
// stored body says how many bytes it dropped, so a cut is never mistaken for
// the whole content. Redaction runs BEFORE compression — afterwards the text is
// opaque to the scrubber.

use std::io::{Read, Write};

use anyhow::{anyhow, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::code_studio::redact;

/// Largest input or output kept per step, in bytes of UTF-8 text.
pub const MAX_SIDE_BYTES: usize = 256 * 1024;

const ZSTD_LEVEL: i32 = 6;

/// The two sides of one step, as they are read back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepIoBody {
    pub input: String,
    pub output: String,
}

/// A packed body plus the facts the row keeps in the clear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedStepIo {
    pub z: String,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub truncated: bool,
}

/// Cuts `text` to `MAX_SIDE_BYTES` on a character boundary and says how much
/// went; returns the original byte length too.
fn cap(text: &str) -> (String, bool) {
    if text.len() <= MAX_SIDE_BYTES {
        return (text.to_string(), false);
    }
    let mut end = MAX_SIDE_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let dropped = text.len() - end;
    (
        format!(
            "{}\n[… cut: {dropped} more bytes were not stored]",
            &text[..end]
        ),
        true,
    )
}

/// Redacts, caps and compresses one step's input and output.
pub fn pack(input: &str, output: &str) -> Result<PackedStepIo> {
    let (input_text, input_cut) = cap(&redact::redact_text(input));
    let (output_text, output_cut) = cap(&redact::redact_text(output));
    let body = StepIoBody {
        input: input_text,
        output: output_text,
    };
    let json = serde_json::to_vec(&body)?;
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), ZSTD_LEVEL)?;
    encoder.write_all(&json)?;
    let compressed = encoder.finish()?;
    Ok(PackedStepIo {
        z: B64.encode(compressed),
        input_bytes: input.len() as u64,
        output_bytes: output.len() as u64,
        truncated: input_cut || output_cut,
    })
}

/// Inverse of [`pack`].
pub fn unpack(z: &str) -> Result<StepIoBody> {
    let compressed = B64
        .decode(z)
        .map_err(|e| anyhow!("step io is not base64: {e}"))?;
    let mut json = Vec::new();
    zstd::stream::read::Decoder::new(compressed.as_slice())?.read_to_end(&mut json)?;
    Ok(serde_json::from_slice(&json)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_body_survives_the_round_trip_and_shrinks() {
        let input = "Kontekst: sieć mesh łączy urządzenia. ".repeat(400);
        let packed = pack(&input, "odpowiedź").unwrap();
        assert!(!packed.truncated);
        assert_eq!(packed.input_bytes, input.len() as u64);
        assert!(packed.z.len() < input.len() / 4, "{} bytes", packed.z.len());
        let body = unpack(&packed.z).unwrap();
        assert_eq!(body.input, input);
        assert_eq!(body.output, "odpowiedź");
    }

    #[test]
    fn an_oversized_side_is_cut_on_a_char_boundary_and_marked() {
        let input = "ż".repeat(MAX_SIDE_BYTES);
        let packed = pack(&input, "").unwrap();
        assert!(packed.truncated);
        let body = unpack(&packed.z).unwrap();
        assert!(body.input.contains("were not stored"));
        assert!(body.input.len() < MAX_SIDE_BYTES + 100);
    }

    #[test]
    fn credentials_are_scrubbed_before_compression() {
        let packed = pack(
            "Authorization: Bearer sk-abcdefghijklmnopqrstuvwxyz0123456789",
            "",
        )
        .unwrap();
        let body = unpack(&packed.z).unwrap();
        assert!(!body
            .input
            .contains("sk-abcdefghijklmnopqrstuvwxyz0123456789"));
    }
}
