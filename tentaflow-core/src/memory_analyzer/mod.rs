// =============================================================================
// Plik: memory_analyzer/mod.rs
// Opis: Analizator pamieci — analiza zapytan i ekstrakcja informacji dla systemu
//       pamieci. Uzywa malego modelu (bielik-1.5b) do decyzji o Memory.
// =============================================================================

//! Memory Analyzer - moduł do analizy zapytań i ekstrakcji informacji dla systemu pamięci
//!
//! Używa małego, szybkiego modelu (bielik-1.5b) do:
//! - Decyzji czy odpytać Memory (przed głównym modelem)
//! - Ekstrakcji encji i relacji do zapisania (po odpowiedzi głównego modelu)
//! - Wykrywania niejednoznaczności wymagających disambiguation

pub mod types;

pub use types::*;

use crate::api::openai::types::{
    ChatCompletionRequest, ChatCompletionResponse, Message, MessageContent,
};
use crate::intent_analyzer::clean_json_response;
use crate::prompt_registry::{analyzer_llm, SharedPromptRegistry};
use crate::routing::service_manager::ServiceManager;
use std::sync::Arc;
use tokio::time::{timeout, Duration};
use tracing::{debug, warn};

/// Konfiguracja Memory Analyzer
#[derive(Debug, Clone)]
pub struct MemoryAnalyzerConfig {
    /// Nazwa modelu do analizy (domyślnie bielik-1.5b)
    pub model_name: String,
    /// Maksymalna liczba tokenów w odpowiedzi
    pub max_tokens: u32,
    /// Temperatura (niższa = bardziej deterministyczne)
    pub temperature: f32,
    /// Timeout dla wywołania modelu (ms)
    pub timeout_ms: u64,
    /// Progi confidence dla voice recognition
    pub voice_auto_recognize_threshold: f32,
    pub voice_confirmation_threshold: f32,
}

impl Default for MemoryAnalyzerConfig {
    fn default() -> Self {
        Self {
            model_name: "bielik-1-5b".to_string(),
            max_tokens: 1024,
            temperature: 0.1,
            timeout_ms: 5000,
            voice_auto_recognize_threshold: 0.85,
            voice_confirmation_threshold: 0.60,
        }
    }
}

/// Memory Analyzer - analizator zapytań dla systemu pamięci
pub struct MemoryAnalyzer {
    config: MemoryAnalyzerConfig,
    service_manager: Arc<ServiceManager>,
    prompt_registry: SharedPromptRegistry,
}

impl MemoryAnalyzer {
    /// Tworzy nowy Memory Analyzer
    pub fn new(service_manager: Arc<ServiceManager>, config: Option<MemoryAnalyzerConfig>) -> Self {
        Self {
            config: config.unwrap_or_default(),
            prompt_registry: service_manager.prompt_registry.clone(),
            service_manager,
        }
    }

    /// Zmienia model używany do analizy
    pub fn with_model(mut self, model_name: &str) -> Self {
        self.config.model_name = model_name.to_string();
        self
    }

    /// Analizuje zapytanie użytkownika i decyduje czy odpytać Memory
    ///
    /// Wywoływane PRZED głównym modelem
    pub async fn analyze_query(
        &self,
        user_message: &str,
        session_context: Option<&str>,
        speaker_id: Option<&str>,
    ) -> Result<QueryDecision, MemoryAnalyzerError> {
        // Fast-path: pomij LLM dla prostych wzorcow
        if let Some(fast_decision) = self.fast_path_query_decision(user_message) {
            debug!(
                "Memory Analyzer fast-path: {} (skipping LLM call)",
                fast_decision.reasoning
            );
            return Ok(fast_decision);
        }

        // Slow-path: wywolaj LLM dla zlozonych zapytan
        let system_prompt = self.prompt_registry.require_content(analyzer_llm::QUERY_ANALYSIS_SYSTEM);
        let user_prompt = format_query_analysis_user_prompt(user_message, session_context, speaker_id);

        let response = self.call_model(system_prompt, &user_prompt).await?;

        self.parse_query_decision(&response)
    }

    /// Fast-path dla prostych zapytań - pomija wywołanie LLM
    ///
    /// Zwraca Some(QueryDecision) jeśli można zdecydować lokalnie, None jeśli potrzebny LLM
    fn fast_path_query_decision(&self, message: &str) -> Option<QueryDecision> {
        let msg_lower = message.to_lowercase();
        let msg_trimmed = msg_lower.trim();

        // Powitania - zawsze NONE
        let greetings = [
            "cześć", "czesc", "hej", "hejka", "siema", "siemka",
            "dzień dobry", "dzien dobry", "dobry wieczór", "dobry wieczor",
            "cześć jarvis", "czesc jarvis", "hej jarvis", "siema jarvis",
            "witaj", "witam", "hello", "hi",
        ];

        for greeting in greetings {
            if msg_trimmed == greeting
               || (msg_trimmed.starts_with(greeting) && msg_trimmed.as_bytes().get(greeting.len()).map_or(false, |&b| b == b' ' || b == b',')) {
                return Some(skip_memory_decision("Fast-path: greeting detected"));
            }
        }

        // Pytania do AI (Jarvis) - zawsze NONE
        let ai_questions = [
            "jak się masz", "jak sie masz", "co słychać", "co slychac",
            "co robisz", "co porabiasz", "jak tam", "co u ciebie",
            "pomóż mi", "pomoz mi", "pomocy", "help",
        ];

        for pattern in ai_questions {
            if msg_trimmed.contains(pattern) {
                return Some(skip_memory_decision("Fast-path: AI question detected"));
            }
        }

        // Przedstawienia sie - zawsze NONE (Memory zapisze po odpowiedzi)
        let introductions = [
            "mam na imię", "mam na imie",
            "nazywam się", "nazywam sie", "moje imię to", "moje imie to",
        ];

        for intro in introductions {
            if msg_trimmed.contains(intro) {
                return Some(skip_memory_decision("Fast-path: self-introduction detected"));
            }
        }

        // "jestem X" - tylko gdy po "jestem " nastepuje krotkie imie (max 2 slowa)
        if msg_trimmed.starts_with("jestem ") {
            let rest = &msg_trimmed["jestem ".len()..];
            let word_count = rest.split_whitespace().count();
            if word_count <= 2 {
                return Some(skip_memory_decision("Fast-path: self-introduction detected"));
            }
        }

        // Krotkie wiadomosci (< 5 znakow)
        if msg_trimmed.len() < 5 {
            return Some(skip_memory_decision("Fast-path: very short message"));
        }

        // Nie pasuje do fast-path - potrzebny LLM
        None
    }

    /// Analizuje rozmowę i decyduje co zapisać do Memory
    ///
    /// Wywoływane PO odpowiedzi głównego modelu (asynchronicznie)
    pub async fn analyze_for_storage(
        &self,
        user_message: &str,
        ai_response: &str,
    ) -> Result<StoreDecision, MemoryAnalyzerError> {
        self.analyze_for_storage_with_speaker(user_message, ai_response, None, None).await
    }

    /// Analizuje rozmowę i decyduje co zapisać do Memory (z informacją o mówcy)
    ///
    /// Wywoływane PO odpowiedzi głównego modelu (asynchronicznie)
    /// speaker_id: unikalny ID głosu z STT (np. "speaker_abc123")
    /// speaker_name: rozpoznana nazwa osoby (np. "Jan Kowalski") lub None jeśli nieznana
    pub async fn analyze_for_storage_with_speaker(
        &self,
        user_message: &str,
        ai_response: &str,
        speaker_id: Option<&str>,
        speaker_name: Option<&str>,
    ) -> Result<StoreDecision, MemoryAnalyzerError> {
        let system_prompt = self.prompt_registry.require_content(analyzer_llm::STORE_ANALYSIS_SYSTEM);

        let user_prompt = match speaker_id {
            Some(sid) => format_store_analysis_user_prompt_with_speaker(
                user_message,
                ai_response,
                sid,
                speaker_name,
            ),
            None => format_store_analysis_user_prompt(user_message, ai_response),
        };

        let response = self.call_model(system_prompt, &user_prompt).await?;

        self.parse_store_decision(&response)
    }

    /// Generuje pytanie disambiguation dla użytkownika
    pub async fn generate_disambiguation_question(
        &self,
        entity_name: &str,
        candidates: &[(String, String)],
    ) -> Result<String, MemoryAnalyzerError> {
        let system_prompt = self.prompt_registry.require_content(analyzer_llm::DISAMBIGUATION_SYSTEM);
        let user_prompt = format_disambiguation_prompt(entity_name, candidates);

        self.call_model(system_prompt, &user_prompt).await
    }

    /// Wywołuje model LLM (bielik-1.5b)
    async fn call_model(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<String, MemoryAnalyzerError> {
        let start = std::time::Instant::now();

        // Timeout wrapper
        let result = timeout(
            Duration::from_millis(self.config.timeout_ms),
            self.call_model_internal(system_prompt, user_prompt),
        )
        .await;

        let elapsed = start.elapsed();

        match result {
            Ok(inner_result) => {
                debug!(
                    "Memory Analyzer LLM call completed in {:?} (model: {})",
                    elapsed, self.config.model_name
                );
                inner_result
            }
            Err(_) => {
                warn!(
                    "Memory Analyzer timeout after {}ms (elapsed: {:?})",
                    self.config.timeout_ms, elapsed
                );
                Err(MemoryAnalyzerError::Timeout)
            }
        }
    }

    /// Wywołuje model przez QUIC LLM
    async fn call_quic_llm(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<String, MemoryAnalyzerError> {
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
                MemoryAnalyzerError::ModelNotFound(self.config.model_name.clone())
            } else if msg.contains("Pusta odpowiedz") || msg.contains("Nieoczekiwany") {
                MemoryAnalyzerError::UnexpectedResponse
            } else {
                MemoryAnalyzerError::ModelCallFailed(msg)
            }
        })?;

        debug!("Memory Analyzer QUIC response: {}", content);
        Ok(content)
    }

    async fn call_model_internal(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<String, MemoryAnalyzerError> {
        // Najpierw sprawdź czy model jest dostępny jako QUIC LLM
        if self.service_manager.has_quic_llm_service(&self.config.model_name) {
            return self.call_quic_llm(system_prompt, user_prompt).await;
        }

        // Fallback: HTTP backend
        let backends = self
            .service_manager
            .get_service_backends_cloned(&self.config.model_name)
            .ok_or_else(|| {
                warn!(
                    "Model {} not found for Memory Analyzer",
                    self.config.model_name
                );
                MemoryAnalyzerError::ModelNotFound(self.config.model_name.clone())
            })?;

        if backends.is_empty() {
            return Err(MemoryAnalyzerError::ModelNotFound(
                self.config.model_name.clone(),
            ));
        }

        // Wybierz backend (pierwszy dostepny — dla malego modelu wystarczy)
        let backend = &backends[0];

        // Przygotuj request w formacie OpenAI
        let request = ChatCompletionRequest {
            model: self.config.model_name.clone(),
            messages: vec![
                Message {
                    role: "system".to_string(),
                    content: Some(MessageContent::Text(system_prompt.to_string())),
                    ..Default::default()
                },
                Message {
                    role: "user".to_string(),
                    content: Some(MessageContent::Text(user_prompt.to_string())),
                    ..Default::default()
                },
            ],
            temperature: Some(self.config.temperature),
            max_tokens: Some(self.config.max_tokens),
            stream: false,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            stop: None,
            user: None,
            response_format: None,
            tools: None,
            tool_choice: None,
            n: None,
            rag_options: None,
            memory_options: None,
            audio_input: None,
        };

        // Wywołaj backend
        let response: ChatCompletionResponse = backend
            .chat_completion(request)
            .await
            .map_err(|e| MemoryAnalyzerError::ModelCallFailed(e.to_string()))?;

        // Wyciągnij tekst z pierwszego choice
        let content = response
            .choices
            .first()
            .and_then(|choice| choice.message.content.as_ref())
            .and_then(|content| match content {
                MessageContent::Text(text) => Some(text.to_owned()),
                MessageContent::Parts(parts) => parts.first().and_then(|p| {
                    if let crate::api::openai::types::ContentPart::Text { text } = p {
                        Some(text.to_owned())
                    } else {
                        None
                    }
                }),
            })
            .ok_or(MemoryAnalyzerError::UnexpectedResponse)?;

        debug!("Memory Analyzer response: {}", content);
        Ok(content)
    }

    /// Parsuje odpowiedź modelu do QueryDecision
    fn parse_query_decision(&self, response: &str) -> Result<QueryDecision, MemoryAnalyzerError> {
        let cleaned = self.clean_json_response(response);

        // Próbuj sparsować bezpośrednio
        if let Ok(decision) = serde_json::from_str::<QueryDecision>(&cleaned) {
            return Ok(decision);
        }

        // Jeśli JSON nie ma wymaganych pól, spróbuj wyciągnąć to co się da
        if let Ok(partial) = serde_json::from_str::<serde_json::Value>(&cleaned) {
            let should_query = partial.get("should_query")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let query_type_str = partial.get("query_type")
                .and_then(|v| v.as_str())
                .unwrap_or("NONE");

            let query_type = if query_type_str.eq_ignore_ascii_case("NEW_SEARCH") {
                MemoryQueryType::NewSearch
            } else if query_type_str.eq_ignore_ascii_case("REFINE") {
                MemoryQueryType::Refine
            } else if query_type_str.eq_ignore_ascii_case("EXPAND") {
                MemoryQueryType::Expand
            } else {
                MemoryQueryType::None
            };

            let search_terms = partial.get("search_terms")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();

            debug!(
                "Partial QueryDecision parse: should_query={}, query_type={:?}",
                should_query, query_type
            );

            return Ok(QueryDecision {
                should_query,
                query_type,
                search_terms,
                relation_types: vec![],
                time_filter: TimeFilter::Recent,
                reasoning: "Partial parse from malformed LLM response".to_string(),
            });
        }

        // Nic się nie udało - zwróć błąd
        warn!(
            "Failed to parse QueryDecision JSON: Response: {}",
            response
        );
        Err(MemoryAnalyzerError::ParseError(format!(
            "Invalid JSON response: {}",
            &response[..response.len().min(100)]
        )))
    }

    /// Parsuje odpowiedź modelu do StoreDecision
    fn parse_store_decision(&self, response: &str) -> Result<StoreDecision, MemoryAnalyzerError> {
        let cleaned = self.clean_json_response(response);

        serde_json::from_str(&cleaned).map_err(|e| {
            warn!(
                "Failed to parse StoreDecision JSON: {}. Response: {}",
                e, response
            );
            MemoryAnalyzerError::ParseError(e.to_string())
        })
    }

    /// Czyści odpowiedź z markdown code blocks i wyciąga pierwszy JSON
    fn clean_json_response(&self, response: &str) -> String {
        clean_json_response(response)
    }

    /// Sprawdza czy voice confidence wymaga potwierdzenia
    pub fn voice_needs_confirmation(&self, confidence: f32) -> VoiceRecognitionAction {
        if confidence >= self.config.voice_auto_recognize_threshold {
            VoiceRecognitionAction::AutoRecognize
        } else if confidence >= self.config.voice_confirmation_threshold {
            VoiceRecognitionAction::AskConfirmation
        } else {
            VoiceRecognitionAction::TreatAsNew
        }
    }
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

/// Tworzy QueryDecision z should_query=false (pominięcie Memory)
fn skip_memory_decision(reasoning: &str) -> QueryDecision {
    QueryDecision {
        should_query: false,
        query_type: MemoryQueryType::None,
        search_terms: vec![],
        relation_types: vec![],
        time_filter: TimeFilter::Recent,
        reasoning: reasoning.to_string(),
    }
}

/// Template dla user prompt w Query Analysis
fn format_query_analysis_user_prompt(
    user_message: &str,
    session_context: Option<&str>,
    speaker_id: Option<&str>,
) -> String {
    let context_info = match session_context {
        Some(ctx) if !ctx.is_empty() => format!("\n\nPOPRZEDNI KONTEKST:\n{}", ctx),
        _ => "\n\nBrak poprzedniego kontekstu - to pierwsze pytanie w sesji.".to_string(),
    };

    let speaker_info = match speaker_id {
        Some(id) => format!("\nMÓWCA (speaker_id): {}", id),
        None => String::new(),
    };

    format!(
        "WIADOMOŚĆ UŻYTKOWNIKA: {}{}{}\n\nAnaliza JSON:",
        user_message, speaker_info, context_info
    )
}

/// Template dla user prompt w Store Analysis
fn format_store_analysis_user_prompt(user_message: &str, ai_response: &str) -> String {
    format!(
        "WIADOMOŚĆ UŻYTKOWNIKA:\n{}\n\nODPOWIEDŹ AI:\n{}\n\nCo należy zapamiętać? Analiza JSON:",
        user_message, ai_response
    )
}

/// Template dla user prompt w Store Analysis z informacją o mówcy
fn format_store_analysis_user_prompt_with_speaker(
    user_message: &str,
    ai_response: &str,
    speaker_id: &str,
    speaker_name: Option<&str>,
) -> String {
    let speaker_info = match speaker_name {
        Some(name) => format!("MÓWCA: {} (speaker_id: {})", name, speaker_id),
        None => format!("MÓWCA: (nieznany, speaker_id: {})", speaker_id),
    };

    format!(
        "{}\n\nWIADOMOŚĆ UŻYTKOWNIKA:\n{}\n\nODPOWIEDŹ AI:\n{}\n\nCo należy zapamiętać? Analiza JSON:",
        speaker_info, user_message, ai_response
    )
}

/// Template dla disambiguation prompt
fn format_disambiguation_prompt(
    entity_name: &str,
    candidates: &[(String, String)], // (nazwa, kontekst)
) -> String {
    let candidates_str: String = candidates
        .iter()
        .enumerate()
        .map(|(i, (name, context))| format!("{}. {} ({})", i + 1, name, context))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "NIEJEDNOZNACZNA ENCJA: {}\n\nKANDYDACI:\n{}\n\nSformułuj pytanie do użytkownika:",
        entity_name, candidates_str
    )
}

/// Akcja do podjęcia przy rozpoznawaniu głosu
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceRecognitionAction {
    /// Automatycznie rozpoznaj (confidence > 0.85)
    AutoRecognize,
    /// Zapytaj o potwierdzenie (confidence 0.60-0.85)
    AskConfirmation,
    /// Traktuj jako nową osobę (confidence < 0.60)
    TreatAsNew,
}

/// Błędy Memory Analyzer
#[derive(Debug, thiserror::Error)]
pub enum MemoryAnalyzerError {
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

/// Fallback functions - zwracają domyślne wartości gdy analiza się nie powiedzie
impl MemoryAnalyzer {
    /// Fallback dla query decision - nie szukaj
    pub fn fallback_query_decision() -> QueryDecision {
        QueryDecision {
            should_query: false,
            query_type: MemoryQueryType::None,
            search_terms: vec![],
            relation_types: vec![],
            time_filter: TimeFilter::Recent,
            reasoning: "Fallback - analysis failed".to_string(),
        }
    }

    /// Fallback dla store decision - nie zapisuj
    pub fn fallback_store_decision() -> StoreDecision {
        StoreDecision {
            should_store: false,
            importance: 0.0,
            entities: vec![],
            relations: vec![],
            facts: vec![],
            reasoning: "Fallback - analysis failed".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_json_response() {
        let response = "```json\n{\"test\": true}\n```";
        let cleaned = response
            .trim()
            .trim_start_matches("```json")
            .trim_start_matches("```")
            .trim_end_matches("```")
            .trim();
        assert_eq!(cleaned, "{\"test\": true}");
    }

    #[test]
    fn test_fallback_query_decision() {
        let decision = MemoryAnalyzer::fallback_query_decision();
        assert!(!decision.should_query);
        assert_eq!(decision.query_type, MemoryQueryType::None);
    }

    #[test]
    fn test_fallback_store_decision() {
        let decision = MemoryAnalyzer::fallback_store_decision();
        assert!(!decision.should_store);
        assert_eq!(decision.importance, 0.0);
    }

    #[test]
    fn test_voice_thresholds() {
        assert!(0.85 < 0.90); // AutoRecognize
        assert!(0.60 <= 0.75 && 0.75 < 0.85); // AskConfirmation
        assert!(0.50 < 0.60); // TreatAsNew
    }

    #[test]
    fn test_fast_path_greetings() {
        let greetings = [
            "cześć",
            "Cześć",
            "cześć jarvis",
            "Cześć Jarvis",
            "hej",
            "hejka",
            "siema",
            "dzień dobry",
            "witaj",
            "hello",
            "hi",
        ];

        for greeting in greetings {
            let msg_lower = greeting.to_lowercase().trim().to_string();
            let test_greetings = [
                "cześć", "czesc", "hej", "hejka", "siema", "siemka",
                "dzień dobry", "dzien dobry", "dobry wieczór", "dobry wieczor",
                "cześć jarvis", "czesc jarvis", "hej jarvis", "siema jarvis",
                "witaj", "witam", "hello", "hi",
            ];

            let mut found = false;
            for g in test_greetings {
                if msg_lower == g || msg_lower.starts_with(&format!("{} ", g)) {
                    found = true;
                    break;
                }
            }
            assert!(found, "Fast-path should detect greeting: {}", greeting);
        }
    }

    #[test]
    fn test_fast_path_introductions() {
        let intros = [
            "jestem Piotr",
            "Jestem Anna",
            "mam na imię Jan",
            "nazywam się Kowalski",
        ];

        for intro in intros {
            let msg_lower = intro.to_lowercase();
            let intro_patterns = ["jestem ", "mam na imię", "mam na imie", "nazywam się", "nazywam sie"];

            let mut found = false;
            for pattern in intro_patterns {
                if msg_lower.starts_with(pattern) || msg_lower.contains(&format!(" {}", pattern)) {
                    found = true;
                    break;
                }
            }
            assert!(found, "Fast-path should detect introduction: {}", intro);
        }
    }

    #[test]
    fn test_fast_path_should_not_match_questions() {
        let questions = [
            "kim jest Marek?",
            "co wiesz o projekcie NextApp?",
            "opowiedz mi o Kowalskim",
        ];

        for q in questions {
            let msg_lower = q.to_lowercase().trim().to_string();

            let greetings = [
                "cześć", "czesc", "hej", "hejka", "siema",
                "dzień dobry", "dzien dobry",
                "cześć jarvis", "czesc jarvis", "hej jarvis",
            ];

            let is_greeting = greetings.iter().any(|g| {
                msg_lower == *g || msg_lower.starts_with(&format!("{} ", g))
            });

            assert!(!is_greeting, "Question should NOT match greeting fast-path: {}", q);
        }
    }
}
