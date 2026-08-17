// ===== File: tests.rs — grammar engine unit tests =====

use std::sync::Arc;

use crate::grammar::Grammar;
use crate::vocab::GrammarVocab;
use crate::{program, GrammarProgram};

/// Feed a whole string through the automaton; returns whether every codepoint
/// was accepted and the grammar ended in an accepting state.
fn accepts(g: &Grammar, s: &str) -> bool {
    let mut stacks = g.init_stacks();
    for c in s.chars() {
        stacks = g.accept(&stacks, c as u32);
        if stacks.is_empty() {
            return false;
        }
    }
    Grammar::is_complete(&stacks)
}

/// Whether the automaton accepts `s` as a prefix (does not reject any char).
fn accepts_prefix(g: &Grammar, s: &str) -> bool {
    let mut stacks = g.init_stacks();
    for c in s.chars() {
        stacks = g.accept(&stacks, c as u32);
        if stacks.is_empty() {
            return false;
        }
    }
    true
}

#[test]
fn gbnf_literal_and_alternation() {
    let g = Grammar::from_gbnf(r#"root ::= "yes" | "no""#).unwrap();
    assert!(accepts(&g, "yes"));
    assert!(accepts(&g, "no"));
    assert!(!accepts(&g, "maybe"));
    assert!(!accepts(&g, "ye"));
    assert!(!accepts(&g, "yess"));
}

#[test]
fn gbnf_char_class_and_repeat() {
    let g = Grammar::from_gbnf(r#"root ::= [a-c]+ "!""#).unwrap();
    assert!(accepts(&g, "a!"));
    assert!(accepts(&g, "abcabc!"));
    assert!(!accepts(&g, "!"));
    assert!(!accepts(&g, "abd!"));
}

#[test]
fn gbnf_optional_and_ref() {
    let g = Grammar::from_gbnf(
        r#"root ::= sign [0-9]+
sign ::= "-"?"#,
    )
    .unwrap();
    assert!(accepts(&g, "123"));
    assert!(accepts(&g, "-42"));
    assert!(!accepts(&g, "--1"));
    assert!(!accepts(&g, "abc"));
}

#[test]
fn gbnf_repeat_counts() {
    let g = Grammar::from_gbnf(r#"root ::= [0-9]{2,4}"#).unwrap();
    assert!(!accepts(&g, "1"));
    assert!(accepts(&g, "12"));
    assert!(accepts(&g, "1234"));
    assert!(!accepts(&g, "12345"));
}

#[test]
fn regex_date() {
    let g = Grammar::from_regex(r"\d{4}-\d{2}-\d{2}").unwrap();
    assert!(accepts(&g, "2026-07-18"));
    assert!(!accepts(&g, "2026-7-18"));
    assert!(!accepts(&g, "abcd-ef-gh"));
    assert!(!accepts(&g, "2026-07-181"));
    // Prefix acceptance while incomplete.
    assert!(accepts_prefix(&g, "2026-"));
    assert!(!accepts(&g, "2026-"));
}

#[test]
fn regex_alternation_groups_classes() {
    let g = Grammar::from_regex(r"(cat|dog)[0-9]*").unwrap();
    assert!(accepts(&g, "cat"));
    assert!(accepts(&g, "dog42"));
    assert!(!accepts(&g, "fish"));
    let g2 = Grammar::from_regex(r"[a-z]+@[a-z]+\.(com|org)").unwrap();
    assert!(accepts(&g2, "user@example.com"));
    assert!(accepts(&g2, "a@b.org"));
    assert!(!accepts(&g2, "user@example.net"));
}

#[test]
fn json_schema_object() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "name": {"type": "string"},
            "age": {"type": "integer"}
        },
        "required": ["name", "age"]
    });
    let g = Grammar::from_json_schema(&schema).unwrap();
    assert!(accepts(&g, r#"{"name": "Ada", "age": 36}"#));
    assert!(accepts(&g, r#"{ "name":"x" , "age":0 }"#));
    // Wrong type for age.
    assert!(!accepts(&g, r#"{"name": "Ada", "age": "old"}"#));
    // Missing required key.
    assert!(!accepts(&g, r#"{"name": "Ada"}"#));
    // Not an object.
    assert!(!accepts(&g, r#"[1,2,3]"#));
}

#[test]
fn json_schema_enum_and_array() {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "color": {"enum": ["red", "green"]},
            "tags": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["color", "tags"]
    });
    let g = Grammar::from_json_schema(&schema).unwrap();
    assert!(accepts(&g, r#"{"color": "red", "tags": []}"#));
    assert!(accepts(&g, r#"{"color": "green", "tags": ["a", "b"]}"#));
    assert!(!accepts(&g, r#"{"color": "blue", "tags": []}"#));
    assert!(!accepts(&g, r#"{"color": "red", "tags": [1]}"#));
}

#[test]
fn json_object_any_value() {
    let g = Grammar::json_value();
    assert!(accepts(&g, r#"{"a": [1, 2, {"b": true}], "c": null}"#));
    assert!(accepts(&g, r#""just a string""#));
    assert!(accepts(&g, "123.45"));
    assert!(!accepts(&g, r#"{"a": }"#));
}

#[test]
fn utf8_multibyte_class() {
    // A grammar that accepts one letter in [α-ω] then a digit. Feeding the
    // codepoint directly must work.
    let g = Grammar::from_gbnf(r#"root ::= [α-ω] [0-9]"#).unwrap();
    assert!(accepts(&g, "α1"));
    assert!(accepts(&g, "ω9"));
    assert!(!accepts(&g, "a1"));
}

/// A tiny vocab: byte tokens 0..=255 map to their own single byte, plus a few
/// multi-byte word tokens, plus an EOS id.
fn toy_vocab() -> (
    Vec<Option<Vec<u8>>>,
    u32,
    std::collections::HashMap<String, u32>,
) {
    let mut table: Vec<Option<Vec<u8>>> = Vec::new();
    let mut names = std::collections::HashMap::new();
    // 0..256 single-byte tokens.
    for b in 0u16..256 {
        table.push(Some(vec![b as u8]));
    }
    let add = |s: &str,
               table: &mut Vec<Option<Vec<u8>>>,
               names: &mut std::collections::HashMap<String, u32>| {
        let id = table.len() as u32;
        table.push(Some(s.as_bytes().to_vec()));
        names.insert(s.to_string(), id);
    };
    add("true", &mut table, &mut names);
    add("false", &mut table, &mut names);
    add("\"name\"", &mut table, &mut names);
    // Greek omega as a single 2-byte token, plus a fragment token (first byte).
    add("ω", &mut table, &mut names);
    let frag_id = table.len() as u32;
    table.push(Some(vec![0xCF])); // lead byte of ω (U+03C9 = CF 89)
    names.insert("wfrag".into(), frag_id);
    let eos = table.len() as u32;
    table.push(None); // EOS is special, no bytes
    (table, eos, names)
}

#[test]
fn token_mask_soundness_ascii() {
    let (table, eos, _names) = toy_vocab();
    let g = Grammar::from_gbnf(r#"root ::= "true" | "false""#).unwrap();
    let vocab = Arc::new(GrammarVocab::new(table, &[eos]));
    let prog: GrammarProgram = program(g, vocab.clone());
    let m = prog.matcher();
    // At the start, only bytes that begin "true"/"false" are allowed: 't','f'.
    let mut logits = vec![0.0f32; vocab.len()];
    m.apply_mask(&mut logits);
    assert!(logits[b't' as usize].is_finite());
    assert!(logits[b'f' as usize].is_finite());
    assert!(logits[b'x' as usize] == f32::NEG_INFINITY);
    // EOS not allowed at start (grammar not complete).
    assert!(logits[eos as usize] == f32::NEG_INFINITY);
}

#[test]
fn token_mask_advances_and_completes() {
    let (table, eos, names) = toy_vocab();
    let g = Grammar::from_gbnf(r#"root ::= "true" | "false""#).unwrap();
    let vocab = Arc::new(GrammarVocab::new(table, &[eos]));
    let prog = program(g, vocab.clone());
    let mut m = prog.matcher();
    // The multi-byte word token "true" is allowed and completes the grammar.
    let n = vocab.len();
    let mut logits = vec![0.0f32; n];
    m.apply_mask(&mut logits);
    let true_id = names["true"];
    assert!(
        logits[true_id as usize].is_finite(),
        "`true` token must be allowed"
    );
    m.accept_token(true_id);
    assert!(m.is_complete());
    // Now only EOS is allowed.
    let mut logits2 = vec![0.0f32; n];
    m.apply_mask(&mut logits2);
    assert!(logits2[eos as usize] == 0.0 || logits2[eos as usize].is_finite());
    assert!(logits2[b't' as usize] == f32::NEG_INFINITY);
}

#[test]
fn token_mask_rejects_overlong_lead_fragment() {
    // Regression: a lone 3-byte lead byte (0xE0) must NOT be accepted as a
    // prefix of an ASCII-only grammar. Its real range is U+0800.., never `{`.
    let mut table: Vec<Option<Vec<u8>>> = (0u16..256).map(|b| Some(vec![b as u8])).collect();
    let obj_open = table.len() as u32;
    table.push(Some(b"{".to_vec()));
    let eos = table.len() as u32;
    table.push(None);
    let g = Grammar::from_json_schema(&serde_json::json!({
        "type": "object",
        "properties": {"a": {"type": "integer"}},
        "required": ["a"]
    }))
    .unwrap();
    let vocab = Arc::new(GrammarVocab::new(table, &[eos]));
    let prog = program(g, vocab.clone());
    let m = prog.matcher();
    let mut logits = vec![0.0f32; vocab.len()];
    m.apply_mask(&mut logits);
    assert!(
        logits[obj_open as usize].is_finite(),
        "`{{` must be allowed"
    );
    assert!(logits[0x7B].is_finite(), "single-byte `{{` allowed");
    // 3-byte and 4-byte lead bytes are NOT valid starts here.
    assert!(
        logits[0xE0] == f32::NEG_INFINITY,
        "0xE0 lead must be rejected"
    );
    assert!(
        logits[0xF0] == f32::NEG_INFINITY,
        "0xF0 lead must be rejected"
    );
    assert!(
        logits[0xC3] == f32::NEG_INFINITY,
        "0xC3 lead must be rejected"
    );
}

#[test]
fn token_mask_utf8_fragment() {
    let (table, eos, names) = toy_vocab();
    // Grammar: exactly the omega codepoint.
    let g = Grammar::from_gbnf(r#"root ::= [ω]"#).unwrap();
    let vocab = Arc::new(GrammarVocab::new(table, &[eos]));
    let prog = program(g, vocab.clone());
    let mut m = prog.matcher();
    let n = vocab.len();
    let mut logits = vec![0.0f32; n];
    m.apply_mask(&mut logits);
    // The whole-omega token and its lead-byte fragment are both allowed.
    assert!(logits[names["ω"] as usize].is_finite());
    assert!(logits[names["wfrag"] as usize].is_finite());
    // A random ASCII byte is not.
    assert!(logits[b'z' as usize] == f32::NEG_INFINITY);
    // Feed the fragment, then only 0x89 (the trailing byte of ω) completes it.
    m.accept_token(names["wfrag"]);
    let mut logits2 = vec![0.0f32; n];
    m.apply_mask(&mut logits2);
    assert!(logits2[0x89].is_finite());
    assert!(logits2[0x88] == f32::NEG_INFINITY);
}
