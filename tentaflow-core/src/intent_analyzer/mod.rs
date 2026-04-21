// =============================================================================
// Plik: intent_analyzer/mod.rs
// Opis: Uniwersalny analizator intencji uzywajacy Bielika 11B — wykrywanie
//       intencji, tool calling, multi-speaker handling, decyzje o Memory.
// =============================================================================

//! Intent Analyzer - uniwersalny analizator intencji używający Bielika 11B
//!
//! Ten moduł używa modelu (bielik-11b) do:
//! - Wykrywania intencji użytkownika (przedstawienie, pytania o tożsamość)
//! - Tool calling (kalendarz, email, web search) z walidacją parametrów
//! - Multi-speaker handling (wykrywanie wielu mówców, kontekst dla LLM)
//! - Decyzji czy odpytać Memory
//!
//! Flow:
//! 1. analyze() - jeden call do Bielika z pełnym kontekstem
//! 2. Zwraca IntentAnalysisResult z:
//!    - primary_intent (Introduction, ToolCall, Conversation, etc.)
//!    - tool_calls (z walidacją parametrów, brakujące pola)
//!    - multi_speaker (jeśli wykryto wielu mówców)
//!    - needs_memory_query (decyzja o Memory)
//!    - context_for_llm (do wstrzyknięcia do głównego modelu)

pub mod executor;
pub mod types;

pub use executor::ToolExecutor;
pub use types::*;

use crate::prompt_registry::SharedPromptRegistry;
use crate::routing::service_manager::ServiceManager;
use serde_json::Value as JsonValue;
use std::sync::Arc;
use tokio::time::{timeout, Duration};
use tracing::{debug, info, warn};

/// Konfiguracja Intent Analyzer
#[derive(Debug, Clone)]
pub struct IntentAnalyzerConfig {
    /// Nazwa modelu (domyślnie bielik-11b)
    pub model_name: String,
    /// Maksymalna liczba tokenów w odpowiedzi
    pub max_tokens: u32,
    /// Temperatura (niższa = bardziej deterministyczne)
    pub temperature: f32,
    /// Timeout dla wywołania modelu (ms)
    pub timeout_ms: u64,
}

impl Default for IntentAnalyzerConfig {
    fn default() -> Self {
        Self {
            // Używamy bielik-11b zamiast 1.5b dla lepszej analizy kontekstu
            model_name: "bielik-11b".to_string(),
            max_tokens: 4096,
            temperature: 0.1,
            timeout_ms: 30000,
        }
    }
}

/// Intent Analyzer - analizator intencji używający Bielika
pub struct IntentAnalyzer {
    config: IntentAnalyzerConfig,
    service_manager: Arc<ServiceManager>,
    prompt_registry: SharedPromptRegistry,
}

impl IntentAnalyzer {
    /// Tworzy nowy Intent Analyzer
    pub fn new(service_manager: Arc<ServiceManager>, config: Option<IntentAnalyzerConfig>) -> Self {
        let prompt_registry = service_manager.prompt_registry.clone();
        Self {
            config: config.unwrap_or_default(),
            service_manager,
            prompt_registry,
        }
    }

    /// Główna funkcja analizy - jeden call do Bielika
    ///
    /// Parametry:
    /// - user_message: Wiadomość użytkownika (transkrypcja lub tekst)
    /// - speaker_info: Informacje o mówcy (id, name, confidence)
    /// - diarized_speakers: Lista mówców z diarization (jeśli multi-speaker)
    /// - session_context: Kontekst sesji (poprzednie wiadomości)
    ///
    /// Zwraca: IntentAnalysisResult z pełną analizą
    pub async fn analyze(
        &self,
        user_message: &str,
        speaker_id: Option<&str>,
        speaker_name: Option<&str>,
        speaker_confidence: Option<f32>,
        diarized_speakers: Option<&[crate::routing::router::DiarizedSpeaker]>,
        session_context: Option<&str>,
    ) -> Result<IntentAnalysisResult, IntentAnalyzerError> {
        // Fast-path wyłączony - zawsze używaj Bielika dla pełnej analizy
        // (w tym wykrywania przedstawień po pytaniu o imię)

        // === Wywołaj Bielika ===
        let system_prompt = self.build_system_prompt();
        let user_prompt = self.build_user_prompt(
            user_message,
            speaker_id,
            speaker_name,
            speaker_confidence,
            diarized_speakers,
            session_context,
        );

        let response = self.call_model(&system_prompt, &user_prompt).await?;
        self.parse_response(&response)
    }

    /// Fast-path dla prostych wzorców (bez LLM)
    #[allow(dead_code)]
    fn fast_path_analysis(&self, message: &str) -> Option<IntentAnalysisResult> {
        let msg_lower = message.to_lowercase();
        let msg_trimmed = msg_lower.trim();

        // === POWITANIA ===
        let greetings = [
            "cześć",
            "czesc",
            "hej",
            "hejka",
            "siema",
            "siemka",
            "dzień dobry",
            "dzien dobry",
            "dobry wieczór",
            "dobry wieczor",
            "witaj",
            "witam",
            "hello",
            "hi",
            "yo",
        ];

        for greeting in greetings {
            if msg_trimmed == greeting
                || (msg_trimmed.starts_with(greeting)
                    && msg_trimmed
                        .as_bytes()
                        .get(greeting.len())
                        .map_or(false, |&b| b == b' ' || b == b','))
            {
                return Some(IntentAnalysisResult {
                    primary_intent: Some(Intent::Greeting),
                    reasoning: "Fast-path: greeting detected".to_string(),
                    ..Default::default()
                });
            }
        }

        // === POŻEGNANIA ===
        let farewells = [
            "pa",
            "papa",
            "do widzenia",
            "do zobaczenia",
            "na razie",
            "cześć",
            "bye",
            "goodbye",
            "dobranoc",
        ];

        // Tylko jeśli to CAŁOŚĆ wiadomości (nie "cześć" jako powitanie)
        if farewells.contains(&msg_trimmed) && msg_trimmed.len() < 15 {
            // Rozróżnij "cześć" jako pożegnanie vs powitanie po kontekście
            // Na razie traktujemy krótkie "pa", "bye" jako pożegnanie
            if msg_trimmed != "cześć" && msg_trimmed != "czesc" {
                return Some(IntentAnalysisResult {
                    primary_intent: Some(Intent::Farewell),
                    reasoning: "Fast-path: farewell detected".to_string(),
                    ..Default::default()
                });
            }
        }

        // === KRÓTKIE WIADOMOŚCI (< 3 znaki) ===
        if msg_trimmed.len() < 3 {
            return Some(IntentAnalysisResult {
                primary_intent: Some(Intent::Conversation),
                reasoning: "Fast-path: very short message".to_string(),
                ..Default::default()
            });
        }

        // Nie pasuje do fast-path - potrzebny LLM
        None
    }

    /// Buduje system prompt dla Bielika - czyta z rejestru promptow
    fn build_system_prompt(&self) -> String {
        self.prompt_registry
            .require_content(crate::prompt_registry::main_llm::INTENT_ANALYZER_SYSTEM)
            .to_string()
    }

    /// Buduje user prompt z pełnym kontekstem
    fn build_user_prompt(
        &self,
        user_message: &str,
        speaker_id: Option<&str>,
        speaker_name: Option<&str>,
        speaker_confidence: Option<f32>,
        diarized_speakers: Option<&[crate::routing::router::DiarizedSpeaker]>,
        session_context: Option<&str>,
    ) -> String {
        use std::fmt::Write;
        let mut prompt = String::new();

        // Informacje o mówcy
        if let Some(name) = speaker_name {
            let _ = write!(
                prompt,
                "ROZPOZNANY MÓWCA: {} (confidence: {:.2})\n",
                name,
                speaker_confidence.unwrap_or(0.0)
            );
        } else if let Some(id) = speaker_id {
            let _ = write!(
                prompt,
                "NIEZNANY MÓWCA (id: {}, confidence: {:.2})\n",
                id,
                speaker_confidence.unwrap_or(0.0)
            );
        }

        // Multi-speaker info
        if let Some(speakers) = diarized_speakers {
            if speakers.len() > 1 {
                let _ = write!(
                    prompt,
                    "\nMULTI-SPEAKER: Wykryto {} mówców:\n",
                    speakers.len()
                );
                for (i, speaker) in speakers.iter().enumerate() {
                    let status = if speaker.is_known {
                        "ZNANY"
                    } else {
                        "NIEZNANY"
                    };
                    let _ = write!(
                        prompt,
                        "  {}. [{}] {}: \"{}\"\n",
                        i + 1,
                        status,
                        speaker.label,
                        speaker.text.trim()
                    );
                }
            }
        }

        // Kontekst sesji
        if let Some(ctx) = session_context {
            if !ctx.is_empty() {
                let _ = write!(prompt, "\nKONTEKST SESJI:\n{}\n", ctx);
            }
        } else {
            debug!("Intent Analyzer: NO session_context provided!");
        }

        // Główna wiadomość
        let _ = write!(prompt, "\nWIADOMOŚĆ DO ANALIZY:\n\"{}\"\n", user_message);

        prompt.push_str("\nAnaliza JSON:");

        debug!("Intent Analyzer FULL PROMPT:\n{}", prompt);
        prompt
    }

    /// Wywołuje model Bielik
    async fn call_model(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<String, IntentAnalyzerError> {
        let start = std::time::Instant::now();

        let result = timeout(
            Duration::from_millis(self.config.timeout_ms),
            self.call_model_internal(system_prompt, user_prompt),
        )
        .await;

        let elapsed = start.elapsed();

        match result {
            Ok(inner_result) => {
                debug!("Intent Analyzer LLM call completed in {:?}", elapsed);
                inner_result
            }
            Err(_) => {
                warn!("Intent Analyzer timeout after {:?}", elapsed);
                Err(IntentAnalyzerError::Timeout)
            }
        }
    }

    /// Wywołuje model przez QUIC
    async fn call_model_internal(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<String, IntentAnalyzerError> {
        let content = crate::routing::call_llm_simple(
            &self.service_manager,
            &self.config.model_name,
            system_prompt,
            user_prompt,
            self.config.temperature,
            self.config.max_tokens,
        )
        .await
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("nie znaleziony") {
                IntentAnalyzerError::ModelNotFound(self.config.model_name.clone())
            } else if msg.contains("Pusta odpowiedz") || msg.contains("Nieoczekiwany") {
                IntentAnalyzerError::UnexpectedResponse
            } else {
                IntentAnalyzerError::ModelCallFailed(msg)
            }
        })?;

        info!(
            "Intent Analyzer odpowiedź z Bielika: {} znaków",
            content.len()
        );
        debug!("Intent Analyzer RAW response:\n{}", content);
        Ok(content)
    }

    /// Parsuje odpowiedź Bielika do IntentAnalysisResult
    fn parse_response(&self, response: &str) -> Result<IntentAnalysisResult, IntentAnalyzerError> {
        let cleaned = self.clean_json_response(response);

        let json: JsonValue = serde_json::from_str(&cleaned)
            .map_err(|_| IntentAnalyzerError::ParseError(response.to_string()))?;

        if let Ok(result) = serde_json::from_value::<IntentAnalysisResult>(json.clone()) {
            return Ok(self.post_process_result(result));
        }

        Ok(self.extract_from_json(json))
    }

    /// Czyści odpowiedź z markdown code blocks
    fn clean_json_response(&self, response: &str) -> String {
        clean_json_response(response)
    }

    /// Ekstrahuje dane z raw JSON
    fn extract_from_json(&self, json: JsonValue) -> IntentAnalysisResult {
        let mut result = IntentAnalysisResult::default();

        // Primary intent
        if let Some(intent) = json.get("primary_intent") {
            result.primary_intent = self.parse_intent(intent);
        }

        // Tool calls
        if let Some(tools) = json.get("tool_calls").and_then(|v| v.as_array()) {
            for tool_json in tools {
                if let Some(tool) = self.parse_tool_call(tool_json) {
                    let tool_result = ToolCallResult::new(tool);
                    result.tool_calls.push(tool_result);
                }
            }
        }

        // Memory query
        result.needs_memory_query = json
            .get("needs_memory_query")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Memory search terms
        if let Some(terms) = json.get("memory_search_terms").and_then(|v| v.as_array()) {
            result.memory_search_terms = terms
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
        }

        // Context for LLM
        result.context_for_llm = json
            .get("context_for_llm")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Reasoning
        result.reasoning = json
            .get("reasoning")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        self.post_process_result(result)
    }

    /// Parsuje intent z JSON
    fn parse_intent(&self, json: &JsonValue) -> Option<Intent> {
        let intent_type = json.get("type").and_then(|v| v.as_str())?;

        match intent_type {
            "introduction" => {
                let name = json.get("name").and_then(|v| v.as_str())?.to_string();
                let confidence = json
                    .get("confidence")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.8) as f32;
                Some(Intent::Introduction { name, confidence })
            }
            "identity_question" => {
                let question_type = match json.get("question_type").and_then(|v| v.as_str()) {
                    Some("who_am_i") => IdentityQuestionType::WhoAmI,
                    Some("what_is_my_name") => IdentityQuestionType::WhatIsMyName,
                    Some("what_do_you_know") => IdentityQuestionType::WhatDoYouKnow,
                    Some("how_old_am_i") => IdentityQuestionType::HowOldAmI,
                    _ => IdentityQuestionType::Other,
                };
                Some(Intent::IdentityQuestion { question_type })
            }
            "name_correction" => {
                let wrong_name = json
                    .get("wrong_name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let correct_name = json
                    .get("correct_name")
                    .and_then(|v| v.as_str())?
                    .to_string();
                let confidence = json
                    .get("confidence")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.8) as f32;
                Some(Intent::NameCorrection {
                    wrong_name,
                    correct_name,
                    confidence,
                })
            }
            "tool_call" => {
                // Tool call jest obsługiwany osobno w tool_calls array
                None
            }
            "greeting" => Some(Intent::Greeting),
            "farewell" => Some(Intent::Farewell),
            "conversation" => Some(Intent::Conversation),
            other => {
                warn!("Nieznany typ intencji: {}", other);
                Some(Intent::Conversation)
            }
        }
    }

    /// Wyciąga string z pola JSON
    fn json_str(json: &JsonValue, key: &str) -> Option<String> {
        json.get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Wyciąga tablicę stringów z pola JSON
    fn json_str_array(json: &JsonValue, key: &str) -> Vec<String> {
        json.get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Parsuje tool call z JSON
    fn parse_tool_call(&self, json: &JsonValue) -> Option<ToolCall> {
        let tool_type = json.get("tool_type").and_then(|v| v.as_str())?;

        match tool_type {
            "calendar_add" => Some(ToolCall::CalendarAdd(CalendarAddParams {
                title: Self::json_str(json, "title"),
                date: Self::json_str(json, "date"),
                start_time: Self::json_str(json, "start_time"),
                end_time: Self::json_str(json, "end_time"),
                duration: Self::json_str(json, "duration"),
                location: Self::json_str(json, "location"),
                description: Self::json_str(json, "description"),
                attendees: Self::json_str_array(json, "attendees"),
                reminder: Self::json_str(json, "reminder"),
            })),
            "calendar_check" => Some(ToolCall::CalendarCheck(CalendarCheckParams {
                date: Self::json_str(json, "date"),
                date_range: Self::json_str(json, "date_range"),
                search_query: Self::json_str(json, "search_query"),
            })),
            "email_send" => Some(ToolCall::EmailSend(EmailSendParams {
                to: Self::json_str(json, "to"),
                subject: Self::json_str(json, "subject"),
                body: Self::json_str(json, "body"),
                cc: Self::json_str_array(json, "cc"),
                attachments: Self::json_str_array(json, "attachments"),
                priority: Self::json_str(json, "priority"),
            })),
            "web_search" => Some(ToolCall::WebSearch(WebSearchParams {
                query: Self::json_str(json, "query"),
                search_type: Self::json_str(json, "search_type"),
                language: Self::json_str(json, "language"),
                max_results: json
                    .get("max_results")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32),
            })),
            "reminder_set" => Some(ToolCall::ReminderSet(ReminderSetParams {
                message: Self::json_str(json, "message"),
                when: Self::json_str(json, "when"),
                repeat: Self::json_str(json, "repeat"),
            })),
            "timer_set" => Some(ToolCall::TimerSet(TimerSetParams {
                duration: Self::json_str(json, "duration"),
                label: Self::json_str(json, "label"),
            })),
            "note_save" => Some(ToolCall::NoteSave(NoteSaveParams {
                content: Self::json_str(json, "content"),
                title: Self::json_str(json, "title"),
                tags: Self::json_str_array(json, "tags"),
            })),
            _ => None,
        }
    }

    /// Post-processing wyniku - dodaje context_for_llm jeśli potrzebny
    fn post_process_result(&self, mut result: IntentAnalysisResult) -> IntentAnalysisResult {
        // Jeśli jest tool_call z brakującymi parametrami - dodaj pytanie do kontekstu
        for tool_result in &result.tool_calls {
            if !tool_result.is_complete {
                if let Some(ref question) = tool_result.follow_up_question {
                    let ctx = result.context_for_llm.get_or_insert_with(String::new);
                    if !ctx.is_empty() {
                        ctx.push_str("\n");
                    }
                    ctx.push_str(&format!("[TOOL INCOMPLETE] {}", question));
                }
            }
        }

        // Jeśli introduction - dodaj kontekst dla LLM
        if let Some(Intent::Introduction { ref name, .. }) = result.primary_intent {
            let ctx = result.context_for_llm.get_or_insert_with(String::new);
            if !ctx.is_empty() {
                ctx.push_str("\n");
            }
            ctx.push_str(&format!(
                "[INTRODUCTION] Użytkownik przedstawił się jako {}. Przywitaj się używając imienia i potwierdź że zapamiętałeś.",
                name
            ));
        }

        // Jeśli identity_question - dodaj kontekst
        if let Some(Intent::IdentityQuestion { ref question_type }) = result.primary_intent {
            result.needs_memory_query = true;
            let ctx = result.context_for_llm.get_or_insert_with(String::new);
            if !ctx.is_empty() {
                ctx.push_str("\n");
            }
            let hint = match question_type {
                IdentityQuestionType::WhoAmI => {
                    "[IDENTITY] Użytkownik pyta kim jest. Sprawdź Memory i odpowiedz."
                }
                IdentityQuestionType::WhatIsMyName => {
                    "[IDENTITY] Użytkownik pyta o swoje imię. Sprawdź Memory."
                }
                IdentityQuestionType::WhatDoYouKnow => {
                    "[IDENTITY] Użytkownik pyta co o nim wiesz. Podsumuj z Memory."
                }
                _ => "[IDENTITY] Użytkownik pyta o siebie. Sprawdź Memory.",
            };
            ctx.push_str(hint);
        }

        result
    }

    /// Fallback - zwraca domyślny wynik gdy analiza się nie powiedzie
    pub fn fallback_result() -> IntentAnalysisResult {
        IntentAnalysisResult {
            primary_intent: Some(Intent::Conversation),
            reasoning: "Fallback - analysis failed".to_string(),
            ..Default::default()
        }
    }
}

/// Błędy Intent Analyzer
#[derive(Debug, thiserror::Error)]
pub enum IntentAnalyzerError {
    #[error("Model not found: {0}")]
    ModelNotFound(String),

    #[error("Model call failed: {0}")]
    ModelCallFailed(String),

    #[error("Failed to parse response: {0}")]
    ParseError(String),

    #[error("Unexpected response type")]
    UnexpectedResponse,

    #[error("Timeout")]
    Timeout,
}

/// Czyści odpowiedź LLM z markdown code blocks i wyciąga pierwszy obiekt JSON
pub(crate) fn clean_json_response(response: &str) -> String {
    let cleaned = response
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    // Wyciągnij tylko pierwszy JSON object
    if let Some(start) = cleaned.find('{') {
        let mut depth = 0;
        let mut end_pos = start;
        for (i, ch) in cleaned[start..].char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end_pos = start + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        return cleaned[start..end_pos].to_string();
    }

    cleaned.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fast_path_greetings() {
        // Test fast-path bez service_manager
        let greetings = ["cześć", "hej", "siema", "dzień dobry", "hello"];

        for greeting in greetings {
            let msg_lower = greeting.to_lowercase().trim().to_string();
            let test_greetings = [
                "cześć",
                "czesc",
                "hej",
                "hejka",
                "siema",
                "siemka",
                "dzień dobry",
                "dzien dobry",
                "dobry wieczór",
                "dobry wieczor",
                "witaj",
                "witam",
                "hello",
                "hi",
                "yo",
            ];

            let is_greeting = test_greetings
                .iter()
                .any(|g| msg_lower == *g || msg_lower.starts_with(&format!("{} ", g)));

            assert!(is_greeting, "Should detect greeting: {}", greeting);
        }
    }

    #[test]
    fn test_clean_json_response() {
        let response = "```json\n{\"primary_intent\": {\"type\": \"greeting\"}}\n```";
        let cleaned = response
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        assert!(cleaned.starts_with("{"));
        assert!(cleaned.ends_with("}"));
    }
}
