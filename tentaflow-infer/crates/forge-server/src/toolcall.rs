// ===== File: toolcall.rs — streaming tool-call + <think> parsing of model output =====
// SPEC §8.1.1: models emit tool calls as text; the API must return structured
// `tool_calls`. Every parser here is streaming-incremental with stop-matcher
// style holdback: text that may be the start of a marker is held until the
// marker is confirmed or ruled out, so nothing that belongs to a tool call
// (or a reasoning block) ever leaks into `content`.

/// One structured tool call extracted from model output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedToolCall {
    pub name: String,
    /// Raw JSON text of the arguments object (OpenAI serializes arguments
    /// as a JSON-encoded string, so it is kept unparsed).
    pub arguments: String,
}

/// Result of feeding text into a parser: plain text safe to emit now,
/// reasoning text (from `<think>` blocks), and any completed tool calls.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ParseStep {
    pub text: String,
    pub reasoning: String,
    pub calls: Vec<ParsedToolCall>,
}

impl ParseStep {
    fn merge(&mut self, other: ParseStep) {
        self.text.push_str(&other.text);
        self.reasoning.push_str(&other.reasoning);
        self.calls.extend(other.calls);
    }
}

/// Streaming-incremental tool-call extraction contract.
pub trait ToolCallParser: Send {
    /// Feed a decoded text piece; returns what is now safe to surface.
    fn push(&mut self, text: &str) -> ParseStep;
    /// End of generation: flush all held-back text and finalize.
    fn finish(&mut self) -> ParseStep;
}

/// Which tool-call syntax the served model emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolParserKind {
    Hermes,
    Llama3Json,
    Muse,
    None,
}

impl ToolParserKind {
    /// Resolve the parser for a served model: explicit config override first
    /// (unconditional), then auto-detection — but ONLY when the resolved
    /// template actually references tools. A template that never renders
    /// tool definitions means the model cannot see them, so parsing its
    /// output for calls would misclassify ordinary JSON answers. Mistral
    /// `[TOOL_CALLS]` output parsing is out of scope, so those templates
    /// pass through unchanged.
    pub fn resolve(
        override_name: Option<&str>,
        arch: &str,
        chat_template: &str,
    ) -> Result<Self, String> {
        if let Some(name) = override_name {
            return match name {
                "hermes" => Ok(Self::Hermes),
                "llama3" => Ok(Self::Llama3Json),
                "muse" => Ok(Self::Muse),
                "none" => Ok(Self::None),
                other => Err(format!(
                    "unknown tool_call_parser {other:?}; expected \"hermes\", \"llama3\", \"muse\" or \"none\""
                )),
            };
        }
        if matches!(arch, "muse-glimmer" | "muse_glimmer") {
            return Ok(Self::Muse);
        }
        if !chat_template.contains("tools") {
            return Ok(Self::None);
        }
        if chat_template.contains("[TOOL_CALLS]") {
            return Ok(Self::None);
        }
        if chat_template.contains("<tool_call>") {
            return Ok(Self::Hermes);
        }
        if arch.starts_with("qwen") {
            return Ok(Self::Hermes);
        }
        if arch.starts_with("llama") {
            return Ok(Self::Llama3Json);
        }
        Ok(Self::None)
    }

    fn instantiate(self) -> Box<dyn ToolCallParser> {
        match self {
            Self::Hermes => Box::new(HermesParser::default()),
            Self::Llama3Json => Box::new(Llama3JsonParser::default()),
            Self::Muse => Box::new(PassthroughParser),
            Self::None => Box::new(PassthroughParser),
        }
    }
}

/// Longest suffix of `buf` that is a proper prefix of `marker`; that suffix
/// must be held back because more input could complete the marker. Markers
/// are ASCII, so byte-slicing them is always UTF-8 safe.
fn holdback_len(buf: &str, marker: &str) -> usize {
    let max = buf.len().min(marker.len() - 1);
    (1..=max)
        .rev()
        .find(|&k| buf.as_bytes().ends_with(&marker.as_bytes()[..k]))
        .unwrap_or(0)
}

/// Split `buf` at the first occurrence of `marker`: returns the text before
/// it and whether the marker was found. On a hit, `buf` keeps the remainder
/// after the marker; on a miss, `buf` keeps only a possible marker prefix.
fn split_at_marker(buf: &mut String, marker: &str) -> (String, bool) {
    if let Some(pos) = buf.find(marker) {
        let after = buf[pos + marker.len()..].to_string();
        let before = buf[..pos].to_string();
        *buf = after;
        return (before, true);
    }
    let hold = holdback_len(buf, marker);
    let keep = buf.split_off(buf.len() - hold);
    (std::mem::replace(buf, keep), false)
}

const TOOL_OPEN: &str = "<tool_call>";
const TOOL_CLOSE: &str = "</tool_call>";

/// Hermes/Qwen style: `<tool_call>\n{"name": ..., "arguments": ...}\n</tool_call>`,
/// any number of calls interleaved with plain text.
#[derive(Default)]
pub struct HermesParser {
    buf: String,
    inside: bool,
}

/// Decode the JSON between Hermes markers. `arguments` may arrive as an
/// object (canonical) or as a pre-encoded JSON string; both map to the raw
/// JSON string OpenAI expects.
fn decode_hermes_body(body: &str) -> Option<ParsedToolCall> {
    let v: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    let name = v.get("name")?.as_str()?.to_string();
    let arguments = match v.get("arguments") {
        None | Some(serde_json::Value::Null) => "{}".to_string(),
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    Some(ParsedToolCall { name, arguments })
}

impl ToolCallParser for HermesParser {
    fn push(&mut self, text: &str) -> ParseStep {
        self.buf.push_str(text);
        let mut step = ParseStep::default();
        loop {
            if self.inside {
                let (body, found) = split_at_marker(&mut self.buf, TOOL_CLOSE);
                if !found {
                    // Inner text must never leak; re-buffer it (including a
                    // possible partial close marker) until the close arrives.
                    let held = std::mem::replace(&mut self.buf, body);
                    self.buf.push_str(&held);
                    return step;
                }
                self.inside = false;
                match decode_hermes_body(&body) {
                    Some(call) => step.calls.push(call),
                    None => {
                        tracing::warn!(
                            "malformed tool_call JSON, surfacing as text: {}",
                            body.trim()
                        );
                        step.text.push_str(TOOL_OPEN);
                        step.text.push_str(&body);
                        step.text.push_str(TOOL_CLOSE);
                    }
                }
            } else {
                let (before, found) = split_at_marker(&mut self.buf, TOOL_OPEN);
                step.text.push_str(&before);
                if !found {
                    return step;
                }
                self.inside = true;
            }
        }
    }

    fn finish(&mut self) -> ParseStep {
        let mut step = ParseStep::default();
        let rest = std::mem::take(&mut self.buf);
        if self.inside {
            // Unterminated tool call: surface the raw text rather than drop it.
            tracing::warn!("unterminated <tool_call> at end of generation");
            step.text.push_str(TOOL_OPEN);
        }
        self.inside = false;
        step.text.push_str(&rest);
        step
    }
}

/// Llama-3.x built-in tool syntax: the ENTIRE assistant message is one JSON
/// object `{"name": "...", "parameters": {...}}`. Anything else passes
/// through. Output is buffered only while it is still a valid prefix of a
/// JSON object; the moment that becomes impossible (non-`{` start, or a
/// syntax error before end-of-input) it flushes and passes through for good.
#[derive(Default)]
pub struct Llama3JsonParser {
    buf: String,
    passthrough: bool,
}

/// Can `buf` still grow into (or already be) one complete JSON value?
/// serde's error category distinguishes "ran out of input" (still possible)
/// from a syntax error mid-stream (impossible).
fn json_still_possible(buf: &str) -> bool {
    match serde_json::from_str::<serde::de::IgnoredAny>(buf) {
        Ok(_) => true,
        Err(e) => e.classify() == serde_json::error::Category::Eof,
    }
}

impl ToolCallParser for Llama3JsonParser {
    fn push(&mut self, text: &str) -> ParseStep {
        let mut step = ParseStep::default();
        if self.passthrough {
            step.text.push_str(text);
            return step;
        }
        self.buf.push_str(text);
        let trimmed = self.buf.trim_start();
        let keep_buffering = match trimmed.chars().next() {
            None => true,
            Some('{') => json_still_possible(trimmed),
            Some(_) => false,
        };
        if !keep_buffering {
            self.passthrough = true;
            step.text.push_str(&std::mem::take(&mut self.buf));
        }
        step
    }

    fn finish(&mut self) -> ParseStep {
        let mut step = ParseStep::default();
        let buf = std::mem::take(&mut self.buf);
        self.passthrough = false;
        let parsed = serde_json::from_str::<serde_json::Value>(buf.trim())
            .ok()
            .and_then(|v| {
                let name = v.get("name")?.as_str()?.to_string();
                // Llama 3.x uses "parameters"; some fine-tunes emit
                // "arguments" — accept both, parameters first.
                let args = v.get("parameters").or_else(|| v.get("arguments"))?;
                args.is_object().then(|| ParsedToolCall {
                    name,
                    arguments: args.to_string(),
                })
            });
        match parsed {
            Some(call) => step.calls.push(call),
            None => step.text.push_str(&buf),
        }
        step
    }
}

/// No tool syntax: everything is plain text.
pub struct PassthroughParser;

impl ToolCallParser for PassthroughParser {
    fn push(&mut self, text: &str) -> ParseStep {
        ParseStep {
            text: text.to_string(),
            ..ParseStep::default()
        }
    }

    fn finish(&mut self) -> ParseStep {
        ParseStep::default()
    }
}

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

/// Streaming `<think>...</think>` extraction (qwen3 reasoning). Block bodies
/// go to `reasoning`, everything else to the content channel. Marker
/// prefixes are held back exactly like tool-call markers.
#[derive(Default)]
struct ThinkExtractor {
    buf: String,
    inside: bool,
}

const MUSE_REASONING: &str = "assistant to=self";
const MUSE_TEXT: &str = "assistant to=user";

#[derive(Default)]
struct MuseChannelExtractor {
    buf: String,
    channel: MuseChannel,
    seen_reasoning: bool,
}

#[derive(Default)]
enum MuseChannel {
    #[default]
    Text,
    Reasoning,
    Ignore,
}

impl MuseChannelExtractor {
    fn push(&mut self, text: &str) -> (String, String) {
        self.buf.push_str(text);
        let (mut content, mut reasoning) = (String::new(), String::new());
        loop {
            let first = [MUSE_REASONING, MUSE_TEXT]
                .iter()
                .filter_map(|marker| self.buf.find(marker).map(|position| (position, *marker)))
                .min_by_key(|(position, _)| *position);
            let Some((position, marker)) = first else {
                let hold = [MUSE_REASONING, MUSE_TEXT]
                    .iter()
                    .map(|marker| holdback_len(&self.buf, marker))
                    .max()
                    .unwrap_or(0);
                let tail = self.buf.split_off(self.buf.len() - hold);
                let before = std::mem::replace(&mut self.buf, tail);
                match self.channel {
                    MuseChannel::Text => content.push_str(&before),
                    MuseChannel::Reasoning => reasoning.push_str(&before),
                    MuseChannel::Ignore => {}
                }
                return (content, reasoning);
            };
            let before = self.buf[..position].to_string();
            self.buf = self.buf[position + marker.len()..].to_string();
            match self.channel {
                MuseChannel::Text => content.push_str(&before),
                MuseChannel::Reasoning => reasoning.push_str(&before),
                MuseChannel::Ignore => {}
            }
            self.channel = if marker == MUSE_REASONING {
                self.seen_reasoning = true;
                MuseChannel::Reasoning
            } else if self.seen_reasoning {
                MuseChannel::Ignore
            } else {
                MuseChannel::Text
            };
        }
    }

    fn finish(&mut self) -> (String, String) {
        let rest = std::mem::take(&mut self.buf);
        self.seen_reasoning = false;
        match std::mem::replace(&mut self.channel, MuseChannel::Text) {
            MuseChannel::Text => (rest, String::new()),
            MuseChannel::Reasoning => (String::new(), rest),
            MuseChannel::Ignore => (String::new(), String::new()),
        }
    }
}

enum ReasoningExtractor {
    Think(ThinkExtractor),
    Muse(MuseChannelExtractor),
}

impl ReasoningExtractor {
    fn push(&mut self, text: &str) -> (String, String) {
        match self {
            Self::Think(extractor) => extractor.push(text),
            Self::Muse(extractor) => extractor.push(text),
        }
    }

    fn finish(&mut self) -> (String, String) {
        match self {
            Self::Think(extractor) => extractor.finish(),
            Self::Muse(extractor) => extractor.finish(),
        }
    }
}

impl ThinkExtractor {
    /// Returns (content_text, reasoning_text) safe to surface now.
    fn push(&mut self, text: &str) -> (String, String) {
        self.buf.push_str(text);
        let (mut content, mut reasoning) = (String::new(), String::new());
        loop {
            let marker = if self.inside { THINK_CLOSE } else { THINK_OPEN };
            let (before, found) = split_at_marker(&mut self.buf, marker);
            if self.inside {
                reasoning.push_str(&before);
            } else {
                content.push_str(&before);
            }
            if !found {
                return (content, reasoning);
            }
            self.inside = !self.inside;
        }
    }

    fn finish(&mut self) -> (String, String) {
        // A held marker prefix was never completed; it belongs to whichever
        // channel we are currently in. An unterminated <think> keeps its
        // whole tail as reasoning (the model ran out of budget mid-thought).
        let rest = std::mem::take(&mut self.buf);
        let inside = std::mem::replace(&mut self.inside, false);
        if inside {
            (String::new(), rest)
        } else {
            (rest, String::new())
        }
    }
}

/// Full output pipeline for one generation: `<think>` extraction feeding a
/// tool-call parser. Content order is preserved; reasoning never reaches the
/// tool parser (Hermes markers inside a think block stay reasoning).
pub struct OutputParser {
    reasoning: ReasoningExtractor,
    tool: Box<dyn ToolCallParser>,
}

impl OutputParser {
    pub fn new(kind: ToolParserKind) -> Self {
        Self {
            reasoning: if kind == ToolParserKind::Muse {
                ReasoningExtractor::Muse(MuseChannelExtractor::default())
            } else {
                ReasoningExtractor::Think(ThinkExtractor::default())
            },
            tool: kind.instantiate(),
        }
    }

    pub fn push(&mut self, text: &str) -> ParseStep {
        let (content, reasoning) = self.reasoning.push(text);
        let mut step = self.tool.push(&content);
        step.reasoning.push_str(&reasoning);
        step
    }

    pub fn finish(&mut self) -> ParseStep {
        let (content, reasoning) = self.reasoning.finish();
        let mut step = self.tool.push(&content);
        step.merge(self.tool.finish());
        step.reasoning.push_str(&reasoning);
        step
    }

    /// Convenience for the non-streaming path: parse a complete output.
    pub fn parse_all(mut self, text: &str) -> ParseStep {
        let mut step = self.push(text);
        step.merge(self.finish());
        step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(parser: &mut OutputParser, pieces: &[&str]) -> ParseStep {
        let mut step = ParseStep::default();
        for p in pieces {
            step.merge(parser.push(p));
        }
        step.merge(parser.finish());
        step
    }

    #[test]
    fn hermes_single_call_with_surrounding_text() {
        let mut p = OutputParser::new(ToolParserKind::Hermes);
        let step = drain(
            &mut p,
            &["I'll check.\n<tool_call>\n{\"name\": \"get_weather\", \"arguments\": {\"city\": \"Kraków\"}}\n</tool_call>\ndone"],
        );
        assert_eq!(step.text, "I'll check.\n\ndone");
        assert_eq!(step.calls.len(), 1);
        assert_eq!(step.calls[0].name, "get_weather");
        let args: serde_json::Value = serde_json::from_str(&step.calls[0].arguments).unwrap();
        assert_eq!(args["city"], "Kraków");
    }

    #[test]
    fn hermes_multiple_calls() {
        let mut p = OutputParser::new(ToolParserKind::Hermes);
        let step = drain(
            &mut p,
            &["<tool_call>{\"name\":\"a\",\"arguments\":{}}</tool_call><tool_call>{\"name\":\"b\",\"arguments\":{\"x\":1}}</tool_call>"],
        );
        assert!(step.text.is_empty());
        assert_eq!(
            step.calls
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(step.calls[1].arguments, "{\"x\":1}");
    }

    #[test]
    fn hermes_malformed_json_surfaces_as_text() {
        let mut p = OutputParser::new(ToolParserKind::Hermes);
        let step = drain(&mut p, &["<tool_call>not json at all</tool_call>after"]);
        assert!(step.calls.is_empty());
        assert_eq!(step.text, "<tool_call>not json at all</tool_call>after");
    }

    #[test]
    fn hermes_marker_split_across_pushes_holds_back() {
        let mut p = OutputParser::new(ToolParserKind::Hermes);
        // Marker arrives byte-dripped across many pushes; nothing of the
        // marker or the call body may leak into content.
        let mut streamed = String::new();
        let mut calls = Vec::new();
        for piece in [
            "Answer: ",
            "<to",
            "ol_c",
            "all>",
            "{\"name\":\"f\",",
            "\"arguments\":{\"q\":\"x\"}}",
            "</tool",
            "_call>",
            " tail",
        ] {
            let step = p.push(piece);
            streamed.push_str(&step.text);
            calls.extend(step.calls);
        }
        let step = p.finish();
        streamed.push_str(&step.text);
        calls.extend(step.calls);
        assert_eq!(streamed, "Answer:  tail");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "f");
        assert_eq!(calls[0].arguments, "{\"q\":\"x\"}");
    }

    #[test]
    fn hermes_false_marker_prefix_is_released() {
        let mut p = OutputParser::new(ToolParserKind::Hermes);
        // "<tool" looks like a marker start but "<tools>" is not one.
        let step = drain(&mut p, &["see <tool", "s> for details"]);
        assert_eq!(step.text, "see <tools> for details");
        assert!(step.calls.is_empty());
    }

    #[test]
    fn hermes_unterminated_call_flushes_raw() {
        let mut p = OutputParser::new(ToolParserKind::Hermes);
        let step = drain(&mut p, &["<tool_call>{\"name\":\"x\""]);
        assert!(step.calls.is_empty());
        assert_eq!(step.text, "<tool_call>{\"name\":\"x\"");
    }

    #[test]
    fn llama3_object_becomes_call() {
        let mut p = OutputParser::new(ToolParserKind::Llama3Json);
        let step = drain(
            &mut p,
            &[
                "{\"name\": \"get_weather\", ",
                "\"parameters\": {\"city\": \"Kraków\"}}",
            ],
        );
        assert!(step.text.is_empty());
        assert_eq!(step.calls.len(), 1);
        assert_eq!(step.calls[0].name, "get_weather");
        let args: serde_json::Value = serde_json::from_str(&step.calls[0].arguments).unwrap();
        assert_eq!(args["city"], "Kraków");
    }

    #[test]
    fn llama3_plain_text_passes_through() {
        let mut p = OutputParser::new(ToolParserKind::Llama3Json);
        let step = drain(&mut p, &["The weather ", "is nice."]);
        assert_eq!(step.text, "The weather is nice.");
        assert!(step.calls.is_empty());
    }

    #[test]
    fn llama3_json_that_is_not_a_tool_call_passes_through() {
        let mut p = OutputParser::new(ToolParserKind::Llama3Json);
        let step = drain(&mut p, &["{\"answer\": 42}"]);
        assert_eq!(step.text, "{\"answer\": 42}");
        assert!(step.calls.is_empty());
    }

    #[test]
    fn llama3_invalid_json_prefix_flushes_immediately() {
        let mut p = OutputParser::new(ToolParserKind::Llama3Json);
        // "{not" can never become valid JSON: it must flush on THIS push,
        // not sit buffered until the end of the stream.
        let step = p.push("{not");
        assert_eq!(step.text, "{not");
        // Everything afterwards streams straight through.
        let step = p.push(" really json");
        assert_eq!(step.text, " really json");
        let step = p.finish();
        assert!(step.text.is_empty() && step.calls.is_empty());
    }

    #[test]
    fn llama3_incomplete_but_valid_prefix_keeps_buffering() {
        let mut p = OutputParser::new(ToolParserKind::Llama3Json);
        // Valid-so-far prefixes (mid-string, mid-object) must be held.
        assert_eq!(p.push("{\"name"), ParseStep::default());
        assert_eq!(p.push("\": \"f\", \"parameters\": {"), ParseStep::default());
        let mut step = p.push("\"a\": 1}}");
        step.merge(p.finish());
        assert!(step.text.is_empty());
        assert_eq!(step.calls.len(), 1);
        assert_eq!(step.calls[0].name, "f");
        assert_eq!(step.calls[0].arguments, "{\"a\":1}");
    }

    #[test]
    fn llama3_complete_object_with_trailing_text_passes_through() {
        let mut p = OutputParser::new(ToolParserKind::Llama3Json);
        let mut text = String::new();
        let mut calls = Vec::new();
        // The trailing text makes the buffer invalid JSON, so the whole
        // thing flushes as content on that push.
        for piece in ["{\"name\": \"f\", \"parameters\": {}}", " and more prose"] {
            let step = p.push(piece);
            text.push_str(&step.text);
            calls.extend(step.calls);
        }
        let step = p.finish();
        text.push_str(&step.text);
        calls.extend(step.calls);
        assert!(calls.is_empty());
        assert_eq!(text, "{\"name\": \"f\", \"parameters\": {}} and more prose");
    }

    #[test]
    fn think_block_streams_to_reasoning() {
        let mut p = OutputParser::new(ToolParserKind::Hermes);
        let mut content = String::new();
        let mut reasoning = String::new();
        for piece in ["<th", "ink>let me ", "reason</think>", "\n\nAnswer."] {
            let step = p.push(piece);
            content.push_str(&step.text);
            reasoning.push_str(&step.reasoning);
        }
        let step = p.finish();
        content.push_str(&step.text);
        reasoning.push_str(&step.reasoning);
        assert_eq!(reasoning, "let me reason");
        assert_eq!(content, "\n\nAnswer.");
    }

    #[test]
    fn tool_call_inside_think_stays_reasoning() {
        let mut p = OutputParser::new(ToolParserKind::Hermes);
        let step = drain(
            &mut p,
            &["<think>maybe <tool_call>{\"name\":\"x\"}</tool_call></think>ok"],
        );
        assert!(step.calls.is_empty());
        assert_eq!(
            step.reasoning,
            "maybe <tool_call>{\"name\":\"x\"}</tool_call>"
        );
        assert_eq!(step.text, "ok");
    }

    #[test]
    fn unterminated_think_is_all_reasoning() {
        let mut p = OutputParser::new(ToolParserKind::Hermes);
        let step = drain(&mut p, &["<think>still going"]);
        assert_eq!(step.reasoning, "still going");
        assert!(step.text.is_empty());
    }

    #[test]
    fn think_then_hermes_call() {
        let mut p = OutputParser::new(ToolParserKind::Hermes);
        let step = drain(
            &mut p,
            &["<think>plan</think>\n<tool_call>{\"name\":\"go\",\"arguments\":{}}</tool_call>"],
        );
        assert_eq!(step.reasoning, "plan");
        assert_eq!(step.text, "\n");
        assert_eq!(step.calls.len(), 1);
        assert_eq!(step.calls[0].name, "go");
    }

    #[test]
    fn muse_channels_are_chunk_safe() {
        let mut p = OutputParser::new(ToolParserKind::Muse);
        let step = drain(
            &mut p,
            &[
                "Wstęp.assistant to=se",
                "lfukryte rozumowanie.assistant to=us",
                "erOdpowiedź dla użytkownika.",
            ],
        );
        assert_eq!(step.reasoning, "ukryte rozumowanie.");
        assert_eq!(step.text, "Wstęp.");
        assert!(step.calls.is_empty());
    }

    #[test]
    fn muse_ignores_user_channel_after_reasoning() {
        let mut p = OutputParser::new(ToolParserKind::Muse);
        let step = drain(
            &mut p,
            &[
                "Pierwsza odpowiedź.assistant to=selfukryte",
                " rozumowanie.assistant to=userPowtórzona odpowiedź.",
            ],
        );
        assert_eq!(step.text, "Pierwsza odpowiedź.");
        assert_eq!(step.reasoning, "ukryte rozumowanie.");
    }

    #[test]
    fn parser_resolution_rules() {
        // Explicit override is unconditional, even on a tool-less template.
        assert_eq!(
            ToolParserKind::resolve(Some("hermes"), "llama", "").unwrap(),
            ToolParserKind::Hermes
        );
        assert_eq!(
            ToolParserKind::resolve(Some("llama3"), "qwen3", "").unwrap(),
            ToolParserKind::Llama3Json
        );
        assert!(ToolParserKind::resolve(Some("bogus"), "qwen3", "").is_err());
        // Auto-detection requires a template that actually references tools;
        // otherwise the model never saw the definitions and plain JSON
        // answers would be misclassified as calls.
        assert_eq!(
            ToolParserKind::resolve(None, "qwen3", "plain template").unwrap(),
            ToolParserKind::None
        );
        assert_eq!(
            ToolParserKind::resolve(None, "llama", "plain template").unwrap(),
            ToolParserKind::None
        );
        assert_eq!(
            ToolParserKind::resolve(None, "qwen3", "{% if tools %}...{% endif %}").unwrap(),
            ToolParserKind::Hermes
        );
        assert_eq!(
            ToolParserKind::resolve(None, "llama", "{% if tools %}...{% endif %}").unwrap(),
            ToolParserKind::Llama3Json
        );
        assert_eq!(
            ToolParserKind::resolve(None, "mistral", "{{ tools }} x [TOOL_CALLS] y").unwrap(),
            ToolParserKind::None
        );
        // Template markers beat family detection.
        assert_eq!(
            ToolParserKind::resolve(None, "weird", "{{ tools }} emit <tool_call> tags").unwrap(),
            ToolParserKind::Hermes
        );
        assert_eq!(
            ToolParserKind::resolve(None, "gemma", "{% if tools %}{% endif %}").unwrap(),
            ToolParserKind::None
        );
        assert_eq!(
            ToolParserKind::resolve(None, "muse_glimmer", "plain template").unwrap(),
            ToolParserKind::Muse
        );
    }
}
