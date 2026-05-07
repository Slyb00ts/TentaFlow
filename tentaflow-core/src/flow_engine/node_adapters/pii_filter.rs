// =============================================================================
// Plik: flow_engine/node_adapters/pii_filter.rs
// Opis: PiiFilterNodeAdapter — pobiera aktywne reguły PII z ctx.pii_rules,
//       aplikuje sekwencyjnie regex replace na envelope.payload (jeśli Text).
//       Plan v4.2 D3 — DbPool wycięty z adaptera, regex compile + cache w
//       impl `PiiRulesStore`.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::stream::{BoxStream, StreamExt};
use regex::Regex;
use regex::RegexBuilder;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, warn};

use crate::flow_engine::envelope::{
    EnvelopeDelta, EnvelopeDeltaKind, FlowEnvelope, FlowValue, LlmStreamChunk, NodeInput,
};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, StreamingNodeAdapter};
use crate::flow_engine::types::{FlowDataType, FlowNode};

const REGEX_SIZE_LIMIT: usize = 1_000_000;

pub struct PiiFilterNodeAdapter;

impl PiiFilterNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for PiiFilterNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

const INPUT_PORTS: &[&str] = &["in"];
/// Stage 3d Krok 2: pii_filter teraz ma `stream` port — streaming chain
/// (LLM → pii_filter → output) propaguje przez `process_stream` zamiast
/// blocking `execute`. R8 typing: oba porty Text→Text.
const OUTPUT_PORTS: &[&str] = &["full", "stream"];

/// Maks bajtów zbierane w buforze przed flush'em jeśli sentence boundary
/// nie pojawi się dłużej. Default 1000 — kompromis: za małe = drobne PII
/// jak email może zostać przerwane mid-token; za duże = klient czeka
/// długo na pierwszy chunk.
const DEFAULT_MAX_BUFFER_CHARS: usize = 1000;
/// Sentence terminators dla streaming flush — parytet z legacy PII
/// `StreamingProcessor` w `services/runtime/middleware.rs`.
const SENTENCE_TERMINATORS: &[char] = &['.', '!', '?', '…', ';', '\n'];

#[async_trait]
impl NodeAdapter for PiiFilterNodeAdapter {
    fn node_type(&self) -> &str {
        "pii_filter"
    }

    fn supported_input_ports(&self) -> &[&'static str] {
        INPUT_PORTS
    }

    fn supported_output_ports(&self) -> &[&'static str] {
        OUTPUT_PORTS
    }

    fn input_port_type(&self, _port: &str) -> FlowDataType {
        FlowDataType::Text
    }

    fn output_port_type(&self, _port: &str) -> FlowDataType {
        FlowDataType::Text
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("pii_filter node requires exactly 1 input edge"))?;

        // Bierzemy text z payload — jeśli payload nie jest Text, pii_filter
        // jest no-op (PII reguły są tekstowe, nie ma sensu próbować na audio
        // czy embeddings). Defensywnie passujemy envelope dalej.
        let mut out = (*input.envelope).clone();
        let mut text = match out.payload {
            FlowValue::Text(ref t) => t.clone(),
            _ => return Ok(out),
        };

        let rules = ctx.pii_rules.active_rules().await?;
        let mut applied = 0u32;
        for rule in &rules {
            match RegexBuilder::new(&rule.pattern)
                .size_limit(REGEX_SIZE_LIMIT)
                .build()
            {
                Ok(re) => {
                    let replaced = re.replace_all(&text, rule.replacement.as_str());
                    if let std::borrow::Cow::Owned(new_text) = replaced {
                        text = new_text;
                        applied += 1;
                        debug!(
                            rule_name = %rule.name,
                            category = %rule.category,
                            "pii_filter: zastosowano regule"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        rule_id = rule.id,
                        rule_name = %rule.name,
                        pattern = %rule.pattern,
                        error = %e,
                        "pii_filter: niepoprawny regex"
                    );
                }
            }
        }

        // Aktualizujemy też ostatnią User message w context.messages, żeby
        // kolejne LLM nody widziały już przefiltrowany input.
        if let Some(last_user) = out
            .context
            .messages
            .iter_mut()
            .rev()
            .find(|m| matches!(m.role, crate::flow_engine::envelope::ChatRole::User))
        {
            // Etap 3b: PII filter operuje wyłącznie na tekście. Multimodal
            // (Parts) message — zostawiamy parts text bez zmiany; image
            // parts nie podlegają regex'om PII.
            use crate::flow_engine::envelope::{ChatMessageContent, MessagePart};
            match &mut last_user.content {
                ChatMessageContent::Text(s) => *s = text.clone(),
                ChatMessageContent::Parts(parts) => {
                    for p in parts.iter_mut() {
                        if let MessagePart::Text { text: t } = p {
                            *t = text.clone();
                            break; // jedna text part w typowym multimodal
                        }
                    }
                }
            }
        }

        out.payload = FlowValue::Text(text);
        out.meta.insert(
            "pii_rules_applied".into(),
            serde_json::json!(applied),
        );
        Ok(out)
    }
}

/// Stage 3d Krok 2: streaming variant pii_filter. Konsumuje upstream
/// `EnvelopeDelta::Llm` deltami, per-choice buffer, sentence-boundary flush,
/// `apply_rules_to_text` (kompilacja regex raz na start, cache w lokalnym
/// stanie), emit cleaned `EnvelopeDelta::Llm` z tym samym `choice_index`.
///
/// Flush warunki:
/// 1. ostatni char delty ∈ SENTENCE_TERMINATORS (sentence boundary)
/// 2. `len(buffer) >= max_buffer_chars` (configurable, default 1000)
/// 3. EOF upstream (final flush)
///
/// `finish_reason` chunki passujemy przez (klient potrzebuje zobaczyć
/// stop/length), ale NAJPIERW flushujemy bufor żeby nie zgubić ostatniego
/// content delta.
#[async_trait]
impl StreamingNodeAdapter for PiiFilterNodeAdapter {
    fn stream_input_kind(&self) -> EnvelopeDeltaKind {
        EnvelopeDeltaKind::Llm
    }
    fn stream_output_kind(&self) -> EnvelopeDeltaKind {
        EnvelopeDeltaKind::Llm
    }

    async fn process_stream(
        &self,
        node: &FlowNode,
        upstream: BoxStream<'static, Result<EnvelopeDelta>>,
        _seed_envelope: Arc<FlowEnvelope>,
        ctx: &ExecutionContext,
    ) -> Result<BoxStream<'static, Result<EnvelopeDelta>>> {
        let max_buffer_chars = node
            .config
            .get("max_buffer_chars")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_BUFFER_CHARS);

        // Pobieramy reguły raz na start streamu — adaptacja do per-chunk
        // gdyby admin zmienił reguły mid-stream nie ma sensu (klient
        // dostaje spójny snapshot reguł dla całego response).
        let raw_rules = ctx.pii_rules.active_rules().await?;
        let compiled: Vec<(String, Regex, String)> = raw_rules
            .into_iter()
            .filter_map(|r| {
                match RegexBuilder::new(&r.pattern)
                    .size_limit(REGEX_SIZE_LIMIT)
                    .build()
                {
                    Ok(re) => Some((r.name, re, r.replacement)),
                    Err(e) => {
                        warn!(
                            rule = %r.name,
                            error = %e,
                            "pii_filter streaming: niepoprawny regex — skip"
                        );
                        None
                    }
                }
            })
            .collect();

        let stream = futures::stream::unfold(
            (
                upstream,
                compiled,
                HashMap::<u32, ChoiceBuffers>::new(),
                max_buffer_chars,
                false,
            ),
            |(mut upstream, compiled, mut buffers, max_chars, mut eof)| async move {
                loop {
                    if eof {
                        // EOF — drain remaining per-choice buffers.
                        if let Some(idx) = buffers.keys().next().copied() {
                            let mut state = buffers.remove(&idx).unwrap();
                            let content_cleaned = if !state.content.is_empty() {
                                Some(apply_rules_to_text(
                                    &std::mem::take(&mut state.content),
                                    &compiled,
                                ))
                            } else {
                                None
                            };
                            let reasoning_cleaned = if !state.reasoning.is_empty() {
                                Some(apply_rules_to_text(
                                    &std::mem::take(&mut state.reasoning),
                                    &compiled,
                                ))
                            } else {
                                None
                            };
                            if content_cleaned.is_none() && reasoning_cleaned.is_none() {
                                continue;
                            }
                            let chunk = LlmStreamChunk {
                                choice_index: idx,
                                text_delta: content_cleaned.unwrap_or_default(),
                                reasoning_delta: reasoning_cleaned,
                                ..Default::default()
                            };
                            return Some((
                                Ok(EnvelopeDelta::Llm(chunk)),
                                (upstream, compiled, buffers, max_chars, eof),
                            ));
                        }
                        return None;
                    }
                    match upstream.next().await {
                        Some(Ok(EnvelopeDelta::Llm(chunk))) => {
                            let idx = chunk.choice_index;
                            let state = buffers.entry(idx).or_default();
                            state.content.push_str(&chunk.text_delta);
                            if let Some(r) = chunk.reasoning_delta.as_deref() {
                                state.reasoning.push_str(r);
                            }
                            // Flush warunki sprawdzane na ostatniej delcie:
                            // sentence terminator w content/reasoning, max
                            // buffer dla któregokolwiek bufora, lub
                            // finish_reason. Wszystkie 3 wymuszają emit.
                            let content_terminator = chunk
                                .text_delta
                                .chars()
                                .any(|c| SENTENCE_TERMINATORS.contains(&c));
                            let reasoning_terminator = chunk
                                .reasoning_delta
                                .as_deref()
                                .map(|r| r.chars().any(|c| SENTENCE_TERMINATORS.contains(&c)))
                                .unwrap_or(false);
                            let over_cap = state.content.len() >= max_chars
                                || state.reasoning.len() >= max_chars;
                            let has_finish = chunk.finish_reason.is_some();
                            if content_terminator || reasoning_terminator || over_cap || has_finish
                            {
                                let drained_content = std::mem::take(&mut state.content);
                                let drained_reasoning = std::mem::take(&mut state.reasoning);
                                let content_cleaned =
                                    apply_rules_to_text(&drained_content, &compiled);
                                let reasoning_cleaned = if drained_reasoning.is_empty() {
                                    None
                                } else {
                                    Some(apply_rules_to_text(&drained_reasoning, &compiled))
                                };
                                let out_chunk = LlmStreamChunk {
                                    choice_index: idx,
                                    text_delta: content_cleaned,
                                    reasoning_delta: reasoning_cleaned,
                                    tool_calls: chunk.tool_calls,
                                    usage: chunk.usage,
                                    finish_reason: chunk.finish_reason,
                                    error: chunk.error,
                                };
                                return Some((
                                    Ok(EnvelopeDelta::Llm(out_chunk)),
                                    (upstream, compiled, buffers, max_chars, eof),
                                ));
                            }
                            continue;
                        }
                        Some(Ok(other)) => {
                            return Some((
                                Ok(other),
                                (upstream, compiled, buffers, max_chars, eof),
                            ));
                        }
                        Some(Err(e)) => {
                            return Some((
                                Err(e),
                                (upstream, compiled, buffers, max_chars, eof),
                            ));
                        }
                        None => {
                            eof = true;
                            continue;
                        }
                    }
                }
            },
        );

        Ok(stream.boxed())
    }
}

/// Per-choice state — osobne bufory dla content i reasoning żeby PII
/// regex aplikował się do całych zdań w każdym kanale niezależnie.
/// Reasoning_content (chain-of-thought od deepseek/o1) niesie wrażliwe
/// dane tak samo jak content — codex review Krok 2a P1.
#[derive(Default)]
struct ChoiceBuffers {
    content: String,
    reasoning: String,
}

fn apply_rules_to_text(text: &str, compiled: &[(String, Regex, String)]) -> String {
    let mut out = text.to_string();
    for (name, re, replacement) in compiled {
        let replaced = re.replace_all(&out, replacement.as_str());
        if let std::borrow::Cow::Owned(new_text) = replaced {
            out = new_text;
            debug!(rule = %name, "pii_filter streaming: applied");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::dispatchers::pii_rules::{PiiRule, PiiRulesStore};
    use crate::flow_engine::envelope::{ChatMessage, ChatRole};
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use anyhow::Result as AnyResult;
    use async_trait::async_trait;
    use std::sync::Arc;

    struct FakePiiRules(Vec<PiiRule>);
    #[async_trait]
    impl PiiRulesStore for FakePiiRules {
        async fn active_rules(&self) -> AnyResult<Vec<PiiRule>> {
            Ok(self.0.clone())
        }
    }

    fn pii_node() -> FlowNode {
        FlowNode {
            id: "pii-1".into(),
            node_type: "pii_filter".into(),
            config: serde_json::Value::Null,
            position: None,
            label: None,
        }
    }

    fn make_input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "src".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn pii_filter_replaces_email_pattern() {
        let mut ctx = stub_ctx();
        ctx.pii_rules = Arc::new(FakePiiRules(vec![PiiRule {
            id: 1,
            name: "email".into(),
            category: "contact".into(),
            pattern: r"[a-z]+@[a-z]+\.com".into(),
            replacement: "[EMAIL]".into(),
        }]));

        let env = FlowEnvelope::with_payload(FlowValue::Text(
            "kontakt: foo@bar.com".into(),
        ));
        let out = PiiFilterNodeAdapter
            .execute(&pii_node(), &[make_input(env)], &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("kontakt: [EMAIL]"));
        assert_eq!(
            out.meta.get("pii_rules_applied").and_then(|v| v.as_u64()),
            Some(1)
        );
    }

    #[tokio::test]
    async fn pii_filter_updates_last_user_message_in_context() {
        let mut ctx = stub_ctx();
        ctx.pii_rules = Arc::new(FakePiiRules(vec![PiiRule {
            id: 1,
            name: "email".into(),
            category: "contact".into(),
            pattern: r"[a-z]+@[a-z]+\.com".into(),
            replacement: "[EMAIL]".into(),
        }]));

        let mut env = FlowEnvelope::with_payload(FlowValue::Text("foo@bar.com".into()));
        env.context.messages.push(ChatMessage::system("be helpful"));
        env.context.messages.push(ChatMessage::user("foo@bar.com"));

        let out = PiiFilterNodeAdapter
            .execute(&pii_node(), &[make_input(env)], &ctx)
            .await
            .unwrap();

        let last_user = out
            .context
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, ChatRole::User))
            .unwrap();
        assert_eq!(last_user.text(), Some("[EMAIL]"));
    }

    #[tokio::test]
    async fn pii_filter_no_op_on_non_text_payload() {
        let env = FlowEnvelope::with_payload(FlowValue::Embedding(vec![0.1, 0.2]));
        let out = PiiFilterNodeAdapter
            .execute(&pii_node(), &[make_input(env)], &stub_ctx())
            .await
            .unwrap();
        // Stub zwraca Vec::new() reguł, więc i tak by było no-op; ale tu
        // testujemy że non-Text payload jest passthrough nawet bez patrzenia
        // w reguły — meta nie dostaje pii_rules_applied.
        assert!(matches!(out.payload, FlowValue::Embedding(_)));
        assert!(out.meta.get("pii_rules_applied").is_none());
    }

    /// Stage 3d Krok 2: streaming pii_filter — sentence boundary flush
    /// łączy delty z 3 chunków ("Jan", " ", "Kowalski.") i aplikuje
    /// regex na cały bufor. Single-chunk PII detection by zgubił to,
    /// bo regex na "Jan" / " " / "Kowalski." osobno nie złapie pełnego
    /// nazwiska.
    #[tokio::test]
    async fn pii_filter_streaming_buffers_until_sentence_boundary() {
        use crate::flow_engine::envelope::FlowEnvelope;
        use futures::stream::StreamExt;

        let mut ctx = stub_ctx();
        ctx.pii_rules = Arc::new(FakePiiRules(vec![PiiRule {
            id: 1,
            name: "full_name".into(),
            category: "pii".into(),
            pattern: r"Jan Kowalski".into(),
            replacement: "[IMIĘ NAZWISKO]".into(),
        }]));

        let upstream = futures::stream::iter(vec![
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: "Jan".into(),
                ..Default::default()
            })),
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: " ".into(),
                ..Default::default()
            })),
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: "Kowalski.".into(), // sentence boundary tu
                ..Default::default()
            })),
        ])
        .boxed();

        let seed = Arc::new(FlowEnvelope::empty());
        let mut out = PiiFilterNodeAdapter
            .process_stream(&pii_node(), upstream, seed, &ctx)
            .await
            .unwrap();

        // Pierwsze dwa delty są buforowane (skip emit). Trzecia ma
        // sentence terminator → flush całego bufora "Jan Kowalski."
        // przez regex → "[IMIĘ NAZWISKO]."
        let chunk = out.next().await.unwrap().unwrap();
        let EnvelopeDelta::Llm(c) = chunk else {
            panic!("expected Llm");
        };
        assert_eq!(c.text_delta, "[IMIĘ NAZWISKO].");
        assert_eq!(c.choice_index, 0);
        // Następne `.next()` → None (stream zakończony, brak EOF drainu
        // bo bufor już opróżniony).
        assert!(out.next().await.is_none());
    }

    /// Stage 3d Krok 2a fix v2: max_buffer_chars wymusza flush nawet bez
    /// sentence terminator'a. Konfiguralnie przez node.config.
    /// Test używa 3 chunków bez terminatora — pierwsze 2 gromadzą się
    /// w buforze, trzeci przekracza cap → flush BEFORE EOF.
    #[tokio::test]
    async fn pii_filter_streaming_max_buffer_flush() {
        use crate::flow_engine::envelope::FlowEnvelope;
        use futures::stream::StreamExt;
        use serde_json::json;

        let mut ctx = stub_ctx();
        ctx.pii_rules = Arc::new(FakePiiRules(vec![]));

        // Każdy delta 8 znaków, 3 razy = 24 chars, cap = 20.
        // Po 3-cim chunk'u bufor osiągnie 24 ≥ 20 → flush.
        let upstream = futures::stream::iter(vec![
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: "abcdefgh".into(), // bufor=8, no flush
                ..Default::default()
            })),
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: "ijklmnop".into(), // bufor=16, no flush
                ..Default::default()
            })),
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: "qrstuvwx".into(), // bufor=24 ≥ cap → FLUSH
                ..Default::default()
            })),
        ])
        .boxed();

        let mut node = pii_node();
        node.config = json!({"max_buffer_chars": 20});

        let seed = Arc::new(FlowEnvelope::empty());
        let mut out = PiiFilterNodeAdapter
            .process_stream(&node, upstream, seed, &ctx)
            .await
            .unwrap();

        // Flush triggerowany przez over_cap PRZED EOF — pierwszy chunk
        // wyemitowany w środku stream'u.
        let chunk = out.next().await.unwrap().unwrap();
        let EnvelopeDelta::Llm(c) = chunk else {
            panic!("expected Llm");
        };
        assert_eq!(c.text_delta, "abcdefghijklmnopqrstuvwx");
        assert_eq!(c.choice_index, 0);
        assert!(out.next().await.is_none(), "stream should EOF after over_cap flush");
    }

    /// Codex review Krok 2a v2 P2: finish_reason w środku stream'u +
    /// buffered content (bez sentence terminator) → forced flush
    /// emituje cleaned content + finish_reason w 1 chunku. Klient
    /// nigdy nie widzi finish PRZED ostatnim content delta.
    #[tokio::test]
    async fn pii_filter_streaming_finish_reason_flushes_buffered_content() {
        use crate::flow_engine::envelope::{FinishReason, FlowEnvelope};
        use futures::stream::StreamExt;

        let mut ctx = stub_ctx();
        ctx.pii_rules = Arc::new(FakePiiRules(vec![PiiRule {
            id: 1,
            name: "secret".into(),
            category: "pii".into(),
            pattern: r"top_secret".into(),
            replacement: "[REDACTED]".into(),
        }]));

        let upstream = futures::stream::iter(vec![
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: "Mam top_".into(), // bufor partial PII, no terminator
                ..Default::default()
            })),
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: "secret".into(), // dalej partial, no terminator
                finish_reason: Some(FinishReason::Stop), // → forced flush
                ..Default::default()
            })),
        ])
        .boxed();

        let seed = Arc::new(FlowEnvelope::empty());
        let mut out = PiiFilterNodeAdapter
            .process_stream(&pii_node(), upstream, seed, &ctx)
            .await
            .unwrap();

        let chunk = out.next().await.unwrap().unwrap();
        let EnvelopeDelta::Llm(c) = chunk else {
            panic!("expected Llm");
        };
        // Cleaned content "Mam [REDACTED]" + finish_reason w jednym chunku.
        assert_eq!(c.text_delta, "Mam [REDACTED]");
        assert_eq!(c.finish_reason, Some(FinishReason::Stop));
        assert!(out.next().await.is_none());
    }

    /// Codex review Krok 2a v2 P2: content + reasoning w tym samym
    /// chunku (LLM emit oba kanały razem). Flush emituje oba pola
    /// cleaned w 1 chunku — bez gubienia żadnego.
    #[tokio::test]
    async fn pii_filter_streaming_flushes_content_and_reasoning_together() {
        use crate::flow_engine::envelope::FlowEnvelope;
        use futures::stream::StreamExt;

        let mut ctx = stub_ctx();
        ctx.pii_rules = Arc::new(FakePiiRules(vec![PiiRule {
            id: 1,
            name: "secret".into(),
            category: "pii".into(),
            pattern: r"top_secret".into(),
            replacement: "[REDACTED]".into(),
        }]));

        let upstream = futures::stream::iter(vec![Ok(EnvelopeDelta::Llm(LlmStreamChunk {
            choice_index: 0,
            text_delta: "Powiem ci top_secret.".into(), // sentence terminator
            reasoning_delta: Some("Myślę o top_secret.".into()), // reasoning też
            ..Default::default()
        }))])
        .boxed();

        let seed = Arc::new(FlowEnvelope::empty());
        let mut out = PiiFilterNodeAdapter
            .process_stream(&pii_node(), upstream, seed, &ctx)
            .await
            .unwrap();

        let chunk = out.next().await.unwrap().unwrap();
        let EnvelopeDelta::Llm(c) = chunk else {
            panic!("expected Llm");
        };
        assert_eq!(c.text_delta, "Powiem ci [REDACTED].");
        assert_eq!(
            c.reasoning_delta.as_deref(),
            Some("Myślę o [REDACTED].")
        );
    }

    /// Stage 3d Krok 2a P1 fix: reasoning_delta też filtrowany przez PII
    /// (osobny per-choice buffer). Test: 2 chunki reasoning bez content,
    /// terminator w drugiej chunkach → flush + cleaned reasoning.
    #[tokio::test]
    async fn pii_filter_streaming_filters_reasoning_delta() {
        use crate::flow_engine::envelope::FlowEnvelope;
        use futures::stream::StreamExt;

        let mut ctx = stub_ctx();
        ctx.pii_rules = Arc::new(FakePiiRules(vec![PiiRule {
            id: 1,
            name: "secret".into(),
            category: "pii".into(),
            pattern: r"top_secret".into(),
            replacement: "[REDACTED]".into(),
        }]));

        let upstream = futures::stream::iter(vec![
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: "".into(),
                reasoning_delta: Some("Myślę o ".into()),
                ..Default::default()
            })),
            Ok(EnvelopeDelta::Llm(LlmStreamChunk {
                choice_index: 0,
                text_delta: "".into(),
                reasoning_delta: Some("top_secret.".into()),
                ..Default::default()
            })),
        ])
        .boxed();

        let seed = Arc::new(FlowEnvelope::empty());
        let mut out = PiiFilterNodeAdapter
            .process_stream(&pii_node(), upstream, seed, &ctx)
            .await
            .unwrap();

        let chunk = out.next().await.unwrap().unwrap();
        let EnvelopeDelta::Llm(c) = chunk else {
            panic!("expected Llm");
        };
        assert_eq!(c.reasoning_delta.as_deref(), Some("Myślę o [REDACTED]."));
    }

    #[tokio::test]
    async fn pii_filter_invalid_regex_skipped_with_warning() {
        let mut ctx = stub_ctx();
        ctx.pii_rules = Arc::new(FakePiiRules(vec![PiiRule {
            id: 1,
            name: "bad".into(),
            category: "x".into(),
            pattern: "[unclosed".into(), // niepoprawny regex
            replacement: "x".into(),
        }]));
        let env = FlowEnvelope::with_payload(FlowValue::Text("payload".into()));
        let out = PiiFilterNodeAdapter
            .execute(&pii_node(), &[make_input(env)], &ctx)
            .await
            .unwrap();
        // Original text intact, applied count = 0 (skipped invalid regex).
        assert_eq!(out.payload.as_text(), Some("payload"));
        assert_eq!(
            out.meta.get("pii_rules_applied").and_then(|v| v.as_u64()),
            Some(0)
        );
    }
}
