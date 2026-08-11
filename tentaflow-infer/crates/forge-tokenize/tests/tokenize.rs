// ===== File: tokenize.rs — integration tests: GGUF vocab rebuild, streaming detok, chat templates =====

use forge_tokenize::{
    builtin_chat_template, resolve_chat_template, ChatMessage, ChatTemplateEngine, GgufVocab,
    StreamDecoder, Tokenizer,
};
use serde_json::json;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const QWEN_SNAPSHOTS: &str =
    "/home/critix/repos/rust/TentaFlow/.runtime/models/models--Qwen--Qwen3.5-0.8B/snapshots";

fn qwen_snapshot_dir() -> Option<PathBuf> {
    let entries = std::fs::read_dir(QWEN_SNAPSHOTS).ok()?;
    entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.join("tokenizer.json").is_file())
}

fn qwen_tokenizer() -> Option<Tokenizer> {
    let dir = qwen_snapshot_dir()?;
    Some(Tokenizer::from_file(dir.join("tokenizer.json")).expect("load qwen tokenizer.json"))
}

/// GPT-2 byte→unicode alphabet (the ByteLevel mapping): printable latin bytes
/// map to themselves, the rest map to U+0100.. in order.
fn gpt2_byte_alphabet() -> [char; 256] {
    let mut direct = [false; 256];
    let printable = (b'!' as usize..=b'~' as usize)
        .chain(0xA1..=0xAC)
        .chain(0xAE..=0xFF);
    for b in printable {
        direct[b] = true;
    }
    let mut out = ['\0'; 256];
    let mut n = 0u32;
    for (b, slot) in out.iter_mut().enumerate() {
        *slot = if direct[b] {
            char::from_u32(b as u32).unwrap()
        } else {
            let c = char::from_u32(256 + n).unwrap();
            n += 1;
            c
        };
    }
    out
}

/// Minimal byte-level GGUF vocab: 256 byte tokens (id == byte value) plus a
/// few merged tokens appended after.
fn byte_level_gguf_vocab(extra_tokens: &[&str], merges: &[&str]) -> GgufVocab {
    let alphabet = gpt2_byte_alphabet();
    let mut tokens: Vec<String> = alphabet.iter().map(|c| c.to_string()).collect();
    tokens.extend(extra_tokens.iter().map(|s| s.to_string()));
    let token_types = vec![1; tokens.len()];
    GgufVocab {
        model: "gpt2".into(),
        pre: "default".into(),
        tokens,
        token_types,
        scores: vec![],
        merges: merges.iter().map(|s| s.to_string()).collect(),
        bos_id: None,
        eos_id: None,
        pad_id: None,
        unk_id: None,
        add_bos: false,
        add_eos: false,
    }
}

// ---------------------------------------------------------------------------
// GGUF gpt2-family reconstruction
// ---------------------------------------------------------------------------

#[test]
fn gguf_gpt2_bpe_merges_apply() {
    let vocab = byte_level_gguf_vocab(&["hi"], &["h i"]);
    let tok = Tokenizer::from_gguf_vocab(&vocab).expect("build gpt2 tokenizer");
    let ids = tok.encode("hi", false).expect("encode");
    assert_eq!(
        ids,
        vec![256],
        "'h i' merge must produce the single 'hi' token"
    );
    assert_eq!(tok.decode(&ids, false).unwrap(), "hi");
    assert_eq!(tok.token_to_piece(256).as_deref(), Some("hi"));
    assert_eq!(tok.token_to_id("hi"), Some(256));
}

#[test]
fn gguf_gpt2_byte_level_roundtrip() {
    let vocab = byte_level_gguf_vocab(&[], &[]);
    let tok = Tokenizer::from_gguf_vocab(&vocab).expect("build gpt2 tokenizer");
    // Pure byte vocab: every input must roundtrip losslessly through bytes.
    for text in [
        "hello world",
        "z\u{017C}\u{00F3}\u{0142}\u{0107}",
        "😀🌍",
        "你好",
        "a\nb\tc",
    ] {
        let ids = tok.encode(text, false).expect("encode");
        assert_eq!(tok.decode(&ids, false).unwrap(), text, "roundtrip {text:?}");
    }
}

#[test]
fn gguf_gpt2_control_tokens_are_special() {
    let mut vocab = byte_level_gguf_vocab(&["<|end|>"], &[]);
    *vocab.token_types.last_mut().unwrap() = 3;
    vocab.eos_id = Some(256);
    let tok = Tokenizer::from_gguf_vocab(&vocab).expect("build");
    assert_eq!(tok.eos_id(), Some(256));
    // Control token must be matched atomically in input text...
    let ids = tok.encode("<|end|>", false).unwrap();
    assert_eq!(ids, vec![256]);
    // ...and skipped when skip_special_tokens is set.
    assert_eq!(tok.decode(&[256], true).unwrap(), "");
    assert_eq!(tok.decode(&[256], false).unwrap(), "<|end|>");
}

#[test]
fn gguf_add_eos_appends_eos_token() {
    let mut vocab = byte_level_gguf_vocab(&["<s>", "</s>"], &[]);
    vocab.token_types[256] = 3;
    vocab.token_types[257] = 3;
    vocab.bos_id = Some(256);
    vocab.eos_id = Some(257);
    vocab.add_bos = true;
    vocab.add_eos = true;
    let tok = Tokenizer::from_gguf_vocab(&vocab).expect("build");
    let ids = tok.encode("hi", true).unwrap();
    assert_eq!(ids.first(), Some(&256), "add_bos must prepend <s>");
    assert_eq!(ids.last(), Some(&257), "add_eos must append </s>");
    // Without special tokens neither is added.
    let ids = tok.encode("hi", false).unwrap();
    assert!(!ids.contains(&256) && !ids.contains(&257));
}

#[test]
fn gguf_add_eos_without_bos() {
    let mut vocab = byte_level_gguf_vocab(&["</s>"], &[]);
    vocab.token_types[256] = 3;
    vocab.eos_id = Some(256);
    vocab.add_eos = true;
    let tok = Tokenizer::from_gguf_vocab(&vocab).expect("build");
    let ids = tok.encode("hi", true).unwrap();
    assert_eq!(ids.last(), Some(&256));
    assert_eq!(ids.len(), 3, "h, i, </s>");
}

#[test]
fn gguf_unknown_pre_scheme_is_rejected() {
    let mut vocab = byte_level_gguf_vocab(&[], &[]);
    vocab.pre = "some-future-scheme".into();
    let err = Tokenizer::from_gguf_vocab(&vocab).unwrap_err();
    assert!(
        err.to_string().contains("some-future-scheme"),
        "unexpected error: {err}"
    );
}

#[test]
fn gguf_supported_pre_schemes_build() {
    // Every implemented pre scheme must produce a working tokenizer (this
    // also proves each ported split regex compiles under fancy-regex).
    for pre in [
        "",
        "default",
        "gpt-2",
        "qwen2",
        "llama-bpe",
        "llama3",
        "tekken",
        "falcon",
        "deepseek-llm",
        "deepseek-coder",
        "deepseek-v3",
        "command-r",
        "starcoder",
        "gpt-4o",
        "glm4",
    ] {
        let mut vocab = byte_level_gguf_vocab(&[], &[]);
        vocab.pre = pre.into();
        let tok = Tokenizer::from_gguf_vocab(&vocab)
            .unwrap_or_else(|e| panic!("pre {pre:?} failed to build: {e}"));
        let text = "Hello, 世界 123!\n  x";
        let ids = tok.encode(text, false).expect("encode");
        assert_eq!(
            tok.decode(&ids, false).unwrap(),
            text,
            "byte-level roundtrip for pre {pre:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// GGUF llama/SPM reconstruction
// ---------------------------------------------------------------------------

fn spm_gguf_vocab() -> GgufVocab {
    // Merge-tree intermediates must exist in vocab for BPE reconstruction.
    let tokens = [
        "<unk>",
        "<s>",
        "</s>",
        "\u{2581}",
        "h",
        "e",
        "l",
        "o",
        "w",
        "r",
        "d",
        "he",
        "hel",
        "hell",
        "hello",
        "wo",
        "wor",
        "worl",
        "world",
        "\u{2581}hello",
        "\u{2581}world",
    ];
    let mut token_types = vec![1; tokens.len()];
    token_types[0] = 2;
    token_types[1] = 3;
    token_types[2] = 3;
    // Shorter pieces get higher scores so their merges fire first.
    let scores: Vec<f32> = tokens.iter().map(|t| -(t.chars().count() as f32)).collect();
    GgufVocab {
        model: "llama".into(),
        pre: String::new(),
        tokens: tokens.iter().map(|s| s.to_string()).collect(),
        token_types,
        scores,
        merges: vec![],
        bos_id: Some(1),
        eos_id: Some(2),
        pad_id: None,
        unk_id: Some(0),
        add_bos: true,
        add_eos: false,
    }
}

#[test]
fn gguf_spm_reconstruction_encodes_and_adds_bos() {
    let tok = Tokenizer::from_gguf_vocab(&spm_gguf_vocab()).expect("build spm tokenizer");
    let ids = tok.encode("hello world", true).expect("encode");
    assert_eq!(ids.first(), Some(&1), "add_bos must prepend <s>");
    let bos = tok.token_to_id("\u{2581}hello").unwrap();
    let word = tok.token_to_id("\u{2581}world").unwrap();
    assert_eq!(&ids[1..], &[bos, word]);
    assert_eq!(tok.decode(&ids, true).unwrap(), "hello world");
    assert_eq!(tok.bos_id(), Some(1));
    assert_eq!(tok.eos_id(), Some(2));
}

#[test]
fn gguf_spm_without_scores_is_rejected() {
    let mut vocab = spm_gguf_vocab();
    vocab.scores.clear();
    let err = Tokenizer::from_gguf_vocab(&vocab).unwrap_err();
    assert!(
        err.to_string().contains("scores"),
        "unexpected error: {err}"
    );
}

// ---------------------------------------------------------------------------
// StreamDecoder — hermetic byte-level tests
// ---------------------------------------------------------------------------

/// Push a UTF-8 string byte-by-byte as byte tokens and assert the decoder
/// emits only complete scalars and reproduces the text exactly.
fn assert_bytewise_stream(text: &str) {
    let vocab = byte_level_gguf_vocab(&[], &[]);
    let tok = Tokenizer::from_gguf_vocab(&vocab).unwrap();
    let mut dec = StreamDecoder::new(&tok, false);
    let mut out = String::new();
    for &b in text.as_bytes() {
        let piece = dec.push(b as u32).expect("push");
        assert!(
            !piece.contains('\u{FFFD}'),
            "replacement char leaked mid-stream for {text:?}"
        );
        out.push_str(&piece);
    }
    out.push_str(&dec.finish().expect("finish"));
    assert_eq!(out, text);
}

#[test]
fn stream_decoder_emoji_split_across_tokens() {
    assert_bytewise_stream("😀");
    assert_bytewise_stream("a😀b");
}

#[test]
fn stream_decoder_cjk() {
    assert_bytewise_stream("你好，世界");
    assert_bytewise_stream("日本語テスト");
}

#[test]
fn stream_decoder_zwj_and_combining_marks() {
    // Family emoji: four emoji joined by ZWJ — 25 bytes, one grapheme.
    assert_bytewise_stream("👩‍👩‍👧‍👧");
    // Combining marks: e + U+0301, o + U+0308.
    assert_bytewise_stream("e\u{0301}o\u{0308}x");
}

#[test]
fn stream_decoder_truncated_scalar_flushes_replacement() {
    let vocab = byte_level_gguf_vocab(&[], &[]);
    let tok = Tokenizer::from_gguf_vocab(&vocab).unwrap();
    let mut dec = StreamDecoder::new(&tok, false);
    // First two bytes of 😀 only — generation stops mid-scalar.
    assert_eq!(dec.push(0xF0).unwrap(), "");
    assert_eq!(dec.push(0x9F).unwrap(), "");
    let tail = dec.finish().unwrap();
    assert!(
        tail.contains('\u{FFFD}'),
        "truncated bytes must surface, got {tail:?}"
    );
}

/// Byte-level piece encoding a literal string (each UTF-8 byte mapped through
/// the GPT-2 byte alphabet).
fn byte_level_piece(text: &str) -> String {
    let alphabet = gpt2_byte_alphabet();
    text.bytes().map(|b| alphabet[b as usize]).collect()
}

#[test]
fn stream_decoder_emits_literal_replacement_char_token() {
    // A token whose piece IS the replacement char (bytes EF BF BD) is real
    // model output, not an incomplete tail — it must be emitted immediately.
    let piece = byte_level_piece("\u{FFFD}");
    let vocab = byte_level_gguf_vocab(&[&piece], &[]);
    let tok = Tokenizer::from_gguf_vocab(&vocab).unwrap();
    let mut dec = StreamDecoder::new(&tok, false);
    assert_eq!(dec.push(b'a' as u32).unwrap(), "a");
    assert_eq!(
        dec.push(256).unwrap(),
        "\u{FFFD}",
        "literal U+FFFD token must not be held back"
    );
    assert_eq!(dec.push(b'b' as u32).unwrap(), "b");
    assert_eq!(dec.finish().unwrap(), "");
}

#[test]
fn stream_decoder_holds_genuinely_split_scalar_but_not_literal_fffd() {
    let piece = byte_level_piece("\u{FFFD}");
    let vocab = byte_level_gguf_vocab(&[&piece], &[]);
    let tok = Tokenizer::from_gguf_vocab(&vocab).unwrap();
    let mut dec = StreamDecoder::new(&tok, false);
    // 😀 = F0 9F 98 80, fed byte by byte: held until the scalar completes.
    assert_eq!(dec.push(0xF0).unwrap(), "");
    assert_eq!(dec.push(0x9F).unwrap(), "");
    assert_eq!(dec.push(0x98).unwrap(), "");
    assert_eq!(dec.push(0x80).unwrap(), "😀");
    // Immediately after, a literal U+FFFD token still flows through.
    assert_eq!(dec.push(256).unwrap(), "\u{FFFD}");
    assert_eq!(dec.finish().unwrap(), "");
}

#[test]
fn stream_decoder_invalid_byte_is_not_held_forever() {
    // A lone continuation byte can never become valid UTF-8 — emit, not hold.
    let vocab = byte_level_gguf_vocab(&[], &[]);
    let tok = Tokenizer::from_gguf_vocab(&vocab).unwrap();
    let mut dec = StreamDecoder::new(&tok, false);
    assert_eq!(dec.push(0x80).unwrap(), "\u{FFFD}");
    assert_eq!(dec.push(b'x' as u32).unwrap(), "x");
    assert_eq!(dec.finish().unwrap(), "");
}

// ---------------------------------------------------------------------------
// Real Qwen tokenizer (gated on local model snapshot)
// ---------------------------------------------------------------------------

#[test]
fn qwen_roundtrip_encode_decode() {
    let Some(tok) = qwen_tokenizer() else { return };
    assert!(tok.vocab_size() > 100_000);
    for text in [
        "Hello, world!",
        "Zażółć gęślą jaźń — 123.45",
        "def main():\n    return {'k': [1, 2]}\n",
        // q + combining acute has no precomposed NFC form, so it survives the
        // tokenizer's NFC normalizer and still exercises combining marks.
        "你好世界 🌍 👩‍👩‍👧‍👧 q\u{0301}",
    ] {
        let ids = tok.encode(text, true).expect("encode");
        assert!(!ids.is_empty());
        assert_eq!(tok.decode(&ids, true).unwrap(), text, "roundtrip {text:?}");
    }
}

#[test]
fn qwen_streaming_matches_full_decode() {
    let Some(tok) = qwen_tokenizer() else { return };
    let text = "Mixed: ASCII, 中文, русский, 🚀🔥, 👩‍👩‍👧‍👧, cafq\u{0301}!";
    let ids = tok.encode(text, false).unwrap();
    let mut dec = StreamDecoder::new(&tok, false);
    let mut out = String::new();
    for id in &ids {
        let piece = dec.push(*id).unwrap();
        assert!(!piece.contains('\u{FFFD}'), "U+FFFD leaked mid-stream");
        out.push_str(&piece);
    }
    out.push_str(&dec.finish().unwrap());
    assert_eq!(out, tok.decode(&ids, false).unwrap());
    assert_eq!(out, text);
}

// ---------------------------------------------------------------------------
// Chat templating — real Qwen3.5 template
// ---------------------------------------------------------------------------

fn qwen_config_template() -> Option<String> {
    let dir = qwen_snapshot_dir()?;
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("tokenizer_config.json")).ok()?)
            .ok()?;
    Some(cfg.get("chat_template")?.as_str()?.to_string())
}

#[test]
fn qwen_template_three_messages_generation_prompt() {
    let Some(dir) = qwen_snapshot_dir() else {
        return;
    };
    let template = std::fs::read_to_string(dir.join("chat_template.jinja")).unwrap();
    let engine = ChatTemplateEngine::new();
    let messages = [
        ChatMessage::text("system", "You are a terse assistant."),
        ChatMessage::text("user", "What is 2+2?"),
        ChatMessage::text("assistant", "4"),
    ];
    let out = engine
        .render(
            &template,
            &messages,
            None,
            true,
            false,
            &serde_json::Map::new(),
        )
        .expect("render qwen chat template");
    assert!(out.starts_with("<|im_start|>system\nYou are a terse assistant.<|im_end|>\n"));
    assert!(out.contains("<|im_start|>user\nWhat is 2+2?<|im_end|>\n"));
    assert!(out.contains("<|im_start|>assistant\n<think>\n\n</think>\n\n4<|im_end|>\n"));
    // enable_thinking undefined → generation prompt closes the think block.
    assert!(out.ends_with("<|im_start|>assistant\n<think>\n\n</think>\n\n"));
}

#[test]
fn qwen_template_with_tools() {
    let Some(template) = qwen_config_template() else {
        return;
    };
    let engine = ChatTemplateEngine::new();
    let messages = [
        ChatMessage::text("system", "Use tools when helpful."),
        ChatMessage::text("user", "Weather in Warsaw?"),
    ];
    let tools = json!([{
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "Get current weather for a city",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }
        }
    }]);
    let out = engine
        .render(
            &template,
            &messages,
            Some(&tools),
            true,
            false,
            &serde_json::Map::new(),
        )
        .expect("render qwen template with tools");
    assert!(out.contains("<tools>"));
    assert!(out.contains("get_weather"));
    assert!(out.contains("Use tools when helpful."));
    assert!(out.contains("<|im_start|>user\nWeather in Warsaw?<|im_end|>\n"));
}

#[test]
fn qwen_template_multipart_content_and_tool_call() {
    let Some(template) = qwen_config_template() else {
        return;
    };
    let engine = ChatTemplateEngine::new();
    let mut assistant = ChatMessage::text("assistant", "");
    assistant.tool_calls = Some(json!([{
        "function": {"name": "get_weather", "arguments": {"city": "Warsaw"}}
    }]));
    let messages = [
        ChatMessage::parts(
            "user",
            vec![json!({"type": "text", "text": "Weather in Warsaw?"})],
        ),
        assistant,
        ChatMessage::text("tool", "{\"temp_c\": 21}"),
    ];
    let out = engine
        .render(
            &template,
            &messages,
            None,
            true,
            false,
            &serde_json::Map::new(),
        )
        .expect("render with multipart content and tool call");
    assert!(out.contains("Weather in Warsaw?"));
    assert!(out.contains("<function=get_weather>"));
    assert!(out.contains("<parameter=city>\nWarsaw\n</parameter>"));
    assert!(out.contains("<tool_response>\n{\"temp_c\": 21}\n</tool_response>"));
}

#[test]
fn qwen_template_raise_exception_propagates() {
    let Some(template) = qwen_config_template() else {
        return;
    };
    let engine = ChatTemplateEngine::new();
    // No user message → the template calls raise_exception.
    let messages = [ChatMessage::text("system", "sys only")];
    let err = engine
        .render(
            &template,
            &messages,
            None,
            true,
            false,
            &serde_json::Map::new(),
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("No user query found"),
        "unexpected error: {err}"
    );
}

// ---------------------------------------------------------------------------
// Chat templating — builtins, pycompat, sandbox
// ---------------------------------------------------------------------------

#[test]
fn builtin_chatml_render() {
    let engine = ChatTemplateEngine::new();
    let messages = [
        ChatMessage::text("system", "sys"),
        ChatMessage::text("user", "hi"),
    ];
    let out = engine
        .render(
            builtin_chat_template("chatml").unwrap(),
            &messages,
            None,
            true,
            false,
            &serde_json::Map::new(),
        )
        .unwrap();
    assert_eq!(
        out,
        "<|im_start|>system\nsys<|im_end|>\n<|im_start|>user\nhi<|im_end|>\n<|im_start|>assistant\n"
    );
}

#[test]
fn builtin_llama3_uses_bos_from_extra_vars() {
    let engine = ChatTemplateEngine::new();
    let messages = [ChatMessage::text("user", "hi")];
    let mut extra = serde_json::Map::new();
    extra.insert("bos_token".into(), json!("<|begin_of_text|>"));
    let out = engine
        .render(
            builtin_chat_template("llama3").unwrap(),
            &messages,
            None,
            true,
            false,
            &extra,
        )
        .unwrap();
    assert!(out
        .starts_with("<|begin_of_text|><|start_header_id|>user<|end_header_id|>\n\nhi<|eot_id|>"));
    assert!(out.ends_with("<|start_header_id|>assistant<|end_header_id|>\n\n"));
}

#[test]
fn builtin_qwen_tool_call_rendering() {
    let engine = ChatTemplateEngine::new();
    let mut assistant = ChatMessage::text("assistant", "");
    assistant.content = None;
    assistant.tool_calls = Some(json!([{
        "function": {"name": "lookup", "arguments": {"q": "rust"}}
    }]));
    let messages = [ChatMessage::text("user", "search rust"), assistant];
    let tools = json!([{"type": "function", "function": {"name": "lookup", "parameters": {}}}]);
    let out = engine
        .render(
            builtin_chat_template("qwen").unwrap(),
            &messages,
            Some(&tools),
            false,
            false,
            &serde_json::Map::new(),
        )
        .unwrap();
    assert!(out.contains("<tool_call>\n{\"name\": \"lookup\", \"arguments\": "));
    assert!(out.contains("\"q\":"));
}

#[test]
fn continue_final_message_truncates_after_content() {
    let engine = ChatTemplateEngine::new();
    let messages = [
        ChatMessage::text("user", "finish this"),
        ChatMessage::text("assistant", "The answer is"),
    ];
    let out = engine
        .render(
            builtin_chat_template("chatml").unwrap(),
            &messages,
            None,
            false,
            true,
            &serde_json::Map::new(),
        )
        .unwrap();
    assert!(out.ends_with("<|im_start|>assistant\nThe answer is"));
}

#[test]
fn continue_and_generation_prompt_conflict() {
    let engine = ChatTemplateEngine::new();
    let messages = [ChatMessage::text("user", "x")];
    let err = engine
        .render(
            builtin_chat_template("chatml").unwrap(),
            &messages,
            None,
            true,
            true,
            &serde_json::Map::new(),
        )
        .unwrap_err();
    assert!(err.to_string().contains("mutually exclusive"));
}

#[test]
fn pycompat_string_methods() {
    let engine = ChatTemplateEngine::new();
    let template =
        "{{ ' a b '.strip() }}|{{ 'x,y,z'.split(',')[1] }}|{{ 'abc'.startswith('ab') }}|\
{{ 'abc'.endswith('bc') }}|{{ 'a-b-c'.replace('-', '+') }}|{{ 'hello world'.title() }}|\
{{ 'xxay'.rstrip('yx') }}|{{ '\\nx\\n'.lstrip('\\n') }}|{{ 'a b  c'.split()|length }}";
    let out = engine
        .render(template, &[], None, false, false, &serde_json::Map::new())
        .unwrap();
    assert_eq!(out, "a b|y|true|true|a+b+c|Hello World|xxa|x\n|3");
}

#[test]
fn strftime_now_renders_year() {
    let engine = ChatTemplateEngine::new();
    let out = engine
        .render(
            "{{ strftime_now('%Y') }}",
            &[],
            None,
            false,
            false,
            &serde_json::Map::new(),
        )
        .unwrap();
    assert_eq!(out.len(), 4);
    assert!(out.chars().all(|c| c.is_ascii_digit()));
}

#[test]
fn fuel_limit_stops_runaway_templates() {
    let engine = ChatTemplateEngine::with_fuel(10_000);
    let err = engine
        .render(
            "{% for i in range(100000) %}x{% endfor %}",
            &[],
            None,
            false,
            false,
            &serde_json::Map::new(),
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("render error"),
        "unexpected: {err}"
    );
}

#[test]
fn render_output_size_is_bounded() {
    let engine = ChatTemplateEngine::new();
    // A single VM instruction that allocates ~64 MB — fuel does not stop it,
    // the bounded writer must.
    let err = engine
        .render(
            "{{ 'x' * 67108864 }}",
            &[],
            None,
            false,
            false,
            &serde_json::Map::new(),
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("output exceeds"),
        "unexpected: {err}"
    );
}

#[test]
fn template_source_size_is_bounded() {
    let engine = ChatTemplateEngine::new();
    let big = "x".repeat(300 * 1024);
    let err = engine
        .render(&big, &[], None, false, false, &serde_json::Map::new())
        .unwrap_err();
    assert!(
        err.to_string().contains("source too large"),
        "unexpected: {err}"
    );
}

#[test]
fn messages_input_size_is_bounded() {
    let engine = ChatTemplateEngine::new();
    let messages = [ChatMessage::text("user", "y".repeat(9 * 1024 * 1024))];
    let err = engine
        .render(
            builtin_chat_template("chatml").unwrap(),
            &messages,
            None,
            false,
            false,
            &serde_json::Map::new(),
        )
        .unwrap_err();
    assert!(
        err.to_string().contains("messages too large"),
        "unexpected: {err}"
    );
}

#[test]
fn template_resolution_priority() {
    let over = "OVERRIDE";
    let cfg = "CONFIG";
    let gguf = "GGUF";
    assert_eq!(
        resolve_chat_template(Some(over), Some(cfg), Some(gguf), Some("chatml")).unwrap(),
        over
    );
    assert_eq!(
        resolve_chat_template(None, Some(cfg), Some(gguf), Some("chatml")).unwrap(),
        cfg
    );
    assert_eq!(
        resolve_chat_template(None, None, Some(gguf), None).unwrap(),
        gguf
    );
    assert_eq!(
        resolve_chat_template(None, None, None, Some("chatml")).unwrap(),
        builtin_chat_template("chatml").unwrap()
    );
    assert!(resolve_chat_template(None, None, None, Some("nope")).is_err());
    assert!(resolve_chat_template(None, None, None, None).is_err());
}

#[test]
fn muse_template_uses_builtin_source() {
    let resolved = resolve_chat_template(
        None,
        None,
        None,
        Some("muse_glimmer"),
    )
    .unwrap();
    let rendered = ChatTemplateEngine::new()
        .render(
            resolved,
            &[ChatMessage::text("user", "test")],
            None,
            true,
            false,
            &serde_json::Map::new(),
        )
        .unwrap();
    assert!(rendered.contains("<|start|>user<|message|>test<|eot|>"));
    assert!(rendered.ends_with("<|start|>assistant to=user<|message|>"));
}

#[test]
fn chat_template_supports_conditional_keyword_arguments() {
    let rendered = ChatTemplateEngine::new()
        .render(
            "{%- set state = namespace(name=tcid if tcid else '') -%}{{ state.name }}",
            &[],
            None,
            false,
            false,
            &serde_json::json!({"tcid": "ok"})
                .as_object()
                .cloned()
                .unwrap(),
        )
        .unwrap();
    assert_eq!(rendered, "ok");
}

// Some checkpoints ship tokenizer.json with baked-in truncation (Bielik:
// max_length 2048). from_file must strip it — context limits belong to the
// engine, and silent prompt clipping corrupts long requests.
#[test]
fn from_file_strips_baked_in_truncation() {
    let path = "/home/critix/repos/rust/TentaFlow/.runtime/models/models--TentaFlow--Bielik-PL-Minitron-7B-NVFP4/snapshots/831550e879fd7d700e3f6d79dffc14373deda3a7/tokenizer.json";
    if !std::path::Path::new(path).exists() {
        eprintln!("skipping: Bielik tokenizer not present");
        return;
    }
    let t = forge_tokenize::Tokenizer::from_file(path).unwrap();
    let long = "słowo testowe ".repeat(2000);
    let ids = t.encode(&long, true).unwrap();
    assert!(
        ids.len() > 4000,
        "truncation not stripped: got {} tokens",
        ids.len()
    );
}
