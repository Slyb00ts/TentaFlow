// =============================================================================
// Plik: tts/piper_tokens.rs
// Opis: Piper `.onnx.json` (phoneme_id_map) -> sherpa `tokens.txt` conversion
//       plus self-healing of cached tokens.txt missing the space token. Pure
//       file I/O — no native sherpa-onnx dependency, so it compiles (and is
//       tested) without the `inference-sherpa` feature.
// =============================================================================

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use tracing::info;

/// Converts Piper `.onnx.json` -> sherpa `tokens.txt`. Piper format:
/// `phoneme_id_map: { "<phoneme>": [<id>, ...] }` — sherpa uses the first ID
/// of the array. sherpa-onnx `ReadTokens` special-cases the space token: a
/// line where only the ID remains after whitespace skipping parses as token
/// " ", so the " " phoneme is emitted like any other (the writer produces the
/// line "  <id>", which round-trips through that parser). Without the " "
/// entry sherpa drops every inter-word space and `token2id.at(' ')` after a
/// '.' phoneme throws std::out_of_range, truncating synthesis. OTHER
/// whitespace-only phonemes (tab/newline/NBSP) are still skipped — the line
/// format cannot represent them unambiguously.
pub fn generate_tokens_from_piper_json(json_path: &Path, out_path: &Path) -> Result<()> {
    let bytes =
        std::fs::read(json_path).with_context(|| format!("read {}", json_path.display()))?;
    let v: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse json {}", json_path.display()))?;
    let map = v
        .get("phoneme_id_map")
        .and_then(|x| x.as_object())
        .ok_or_else(|| anyhow!("brak phoneme_id_map w {}", json_path.display()))?;

    let mut entries: Vec<(String, i64)> = Vec::with_capacity(map.len());
    for (phoneme, ids) in map.iter() {
        if phoneme != " " && (phoneme.is_empty() || phoneme.chars().all(char::is_whitespace)) {
            continue;
        }
        let first_id = ids
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|x| x.as_i64())
            .ok_or_else(|| {
                anyhow!(
                    "phoneme_id_map['{}'] nie jest tablica intow w {}",
                    phoneme,
                    json_path.display()
                )
            })?;
        entries.push((phoneme.clone(), first_id));
    }
    entries.sort_by_key(|(_, id)| *id);

    let mut out = String::with_capacity(entries.len() * 8);
    for (phoneme, id) in &entries {
        out.push_str(phoneme);
        out.push(' ');
        out.push_str(&id.to_string());
        out.push('\n');
    }
    std::fs::write(out_path, out).with_context(|| format!("write {}", out_path.display()))?;
    Ok(())
}

/// Repairs a `tokens.txt` produced by an older build that dropped the space
/// phoneme: when the file has no space-token line and the Piper json maps
/// " ", ONLY tokens.txt is regenerated (onnx/metadata/espeak stay untouched).
/// Returns true when the file was rewritten.
pub fn heal_missing_space_token(tokens_path: &Path, json_path: &Path) -> Result<bool> {
    if !tokens_path.exists() || tokens_file_has_space_token(tokens_path)? {
        return Ok(false);
    }
    if !piper_json_maps_space(json_path)? {
        return Ok(false);
    }
    info!(
        "[sherpa-onnx] {} nie ma tokena spacji — regeneruje z {}",
        tokens_path.display(),
        json_path.display()
    );
    generate_tokens_from_piper_json(json_path, tokens_path)?;
    Ok(true)
}

/// A space-token line under sherpa `ReadTokens` semantics has token text " ",
/// so the written line starts with ' '.
fn tokens_file_has_space_token(tokens_path: &Path) -> Result<bool> {
    let content = std::fs::read_to_string(tokens_path)
        .with_context(|| format!("read {}", tokens_path.display()))?;
    Ok(content.lines().any(|l| l.starts_with(' ')))
}

fn piper_json_maps_space(json_path: &Path) -> Result<bool> {
    let bytes =
        std::fs::read(json_path).with_context(|| format!("read {}", json_path.display()))?;
    let v: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse json {}", json_path.display()))?;
    Ok(v.get("phoneme_id_map")
        .and_then(|x| x.as_object())
        .map(|m| m.contains_key(" "))
        .unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_tokens_emits_space_phoneme_and_skips_other_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("voice.onnx.json");
        // "\t" in the raw string is a JSON escape — parses to a TAB phoneme.
        std::fs::write(
            &json,
            r#"{"phoneme_id_map":{"!":[4],"_":[0]," ":[3],"\t":[9]}}"#,
        )
        .unwrap();
        let out = dir.path().join("tokens.txt");
        generate_tokens_from_piper_json(&json, &out).unwrap();

        let content = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        // Sorted by ID; the space phoneme becomes the line "  3" (sherpa
        // parses it as token " " = 3); the TAB phoneme is skipped.
        assert_eq!(lines, vec!["_ 0", "  3", "! 4"]);
    }

    #[test]
    fn heal_regenerates_tokens_without_space_line() {
        let dir = tempfile::tempdir().unwrap();
        let json = dir.path().join("voice.onnx.json");
        std::fs::write(&json, r#"{"phoneme_id_map":{"_":[0]," ":[3],"!":[4]}}"#).unwrap();
        let tokens = dir.path().join("tokens.txt");
        // tokens.txt from an older build — no space line.
        std::fs::write(&tokens, "_ 0\n! 4\n").unwrap();

        let healed = heal_missing_space_token(&tokens, &json).unwrap();
        assert!(healed, "missing space line + json with \" \" => regenerate");
        let content = std::fs::read_to_string(&tokens).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines, vec!["_ 0", "  3", "! 4"]);
    }

    #[test]
    fn heal_leaves_valid_or_spaceless_voices_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let json_with_space = dir.path().join("a.onnx.json");
        std::fs::write(&json_with_space, r#"{"phoneme_id_map":{"_":[0]," ":[3]}}"#).unwrap();
        let json_without_space = dir.path().join("b.onnx.json");
        std::fs::write(&json_without_space, r#"{"phoneme_id_map":{"_":[0]}}"#).unwrap();

        // Sentinel content differs from generator output — detects any rewrite.
        let tokens = dir.path().join("tokens.txt");
        std::fs::write(&tokens, "custom 7\n  3\n").unwrap();
        assert!(!heal_missing_space_token(&tokens, &json_with_space).unwrap());
        assert_eq!(
            std::fs::read_to_string(&tokens).unwrap(),
            "custom 7\n  3\n",
            "tokens.txt with a space line must not be touched"
        );

        std::fs::write(&tokens, "custom 7\n").unwrap();
        assert!(!heal_missing_space_token(&tokens, &json_without_space).unwrap());
        assert_eq!(
            std::fs::read_to_string(&tokens).unwrap(),
            "custom 7\n",
            "a voice without a space phoneme needs no repair"
        );
    }
}
