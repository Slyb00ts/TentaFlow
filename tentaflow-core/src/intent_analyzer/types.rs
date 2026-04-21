// =============================================================================
// Plik: intent_analyzer/types.rs
// Opis: Typy dla analizatora intencji — Intent, ToolCall, ToolCallResult,
//       MultiSpeakerAnalysis, IntentAnalysisResult.
// =============================================================================

//! Typy dla Intent Analyzer
//!
//! Definiuje struktury dla:
//! - Wykrytych intencji (przedstawienie, pytanie o tożsamość, tool call)
//! - Wywołań narzędzi (kalendarz, email, web search)
//! - Multi-speaker handling

use serde::{Deserialize, Serialize};

// ============================================================================
// INTENT TYPES - główne intencje użytkownika
// ============================================================================

/// Główny typ intencji wykrytej przez Bielika
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Intent {
    /// Użytkownik się przedstawia ("jestem Piotr", "mam na imię Anna")
    Introduction {
        /// Wyekstrahowane imię
        name: String,
        /// Pewność ekstrakcji (0.0-1.0)
        confidence: f32,
    },

    /// Użytkownik pyta o swoją tożsamość ("kim jestem?", "jak mam na imię?")
    IdentityQuestion {
        /// Typ pytania
        question_type: IdentityQuestionType,
    },

    /// Użytkownik koryguje swoje imię ("nie, jestem Marek, nie Piotr")
    NameCorrection {
        /// Błędne imię
        wrong_name: Option<String>,
        /// Poprawne imię
        correct_name: String,
        confidence: f32,
    },

    /// Użytkownik chce użyć narzędzia (kalendarz, email, web)
    ToolCall {
        /// Wywołanie narzędzia
        tool: ToolCall,
    },

    /// Zwykła rozmowa - brak specjalnej intencji
    Conversation,

    /// Powitanie
    Greeting,

    /// Pożegnanie
    Farewell,
}

/// Typ pytania o tożsamość
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum IdentityQuestionType {
    /// "Kim jestem?"
    WhoAmI,
    /// "Jak mam na imię?"
    WhatIsMyName,
    /// "Co o mnie wiesz?"
    WhatDoYouKnow,
    /// "Ile mam lat?"
    HowOldAmI,
    /// Inne pytanie o siebie
    Other,
}

// ============================================================================
// TOOL CALL TYPES - wywołania narzędzi
// ============================================================================

/// Wywołanie narzędzia z parametrami
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "tool_type", rename_all = "snake_case")]
pub enum ToolCall {
    /// Dodaj wydarzenie do kalendarza
    CalendarAdd(CalendarAddParams),

    /// Sprawdź kalendarz
    CalendarCheck(CalendarCheckParams),

    /// Wyślij email
    EmailSend(EmailSendParams),

    /// Przeszukaj internet
    WebSearch(WebSearchParams),

    /// Ustaw przypomnienie
    ReminderSet(ReminderSetParams),

    /// Ustaw timer/alarm
    TimerSet(TimerSetParams),

    /// Notatka do zapisania
    NoteSave(NoteSaveParams),
}

/// Parametry dla CalendarAdd
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CalendarAddParams {
    /// Tytuł wydarzenia (wymagane)
    pub title: Option<String>,
    /// Data (wymagane) - format: "2024-01-15" lub "jutro", "w piątek"
    pub date: Option<String>,
    /// Godzina rozpoczęcia - format: "14:00" lub "po południu"
    pub start_time: Option<String>,
    /// Godzina zakończenia
    pub end_time: Option<String>,
    /// Czas trwania (jeśli brak end_time) - "1h", "30min"
    pub duration: Option<String>,
    /// Lokalizacja
    pub location: Option<String>,
    /// Opis/notatki
    pub description: Option<String>,
    /// Uczestnicy (emaile lub imiona)
    #[serde(default)]
    pub attendees: Vec<String>,
    /// Przypomnienie przed wydarzeniem - "15min", "1h", "1d"
    pub reminder: Option<String>,
}

impl CalendarAddParams {
    /// Zwraca listę brakujących wymaganych pól
    pub fn missing_required(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.title.is_none() {
            missing.push("title");
        }
        if self.date.is_none() {
            missing.push("date");
        }
        missing
    }

    /// Czy wszystkie wymagane pola są wypełnione
    pub fn is_complete(&self) -> bool {
        self.title.is_some() && self.date.is_some()
    }
}

/// Parametry dla CalendarCheck
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CalendarCheckParams {
    /// Data do sprawdzenia - "dziś", "jutro", "2024-01-15"
    pub date: Option<String>,
    /// Zakres dat - "ten tydzień", "styczeń"
    pub date_range: Option<String>,
    /// Szukaj konkretnego wydarzenia
    pub search_query: Option<String>,
}

/// Parametry dla EmailSend
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EmailSendParams {
    /// Adresat (wymagane) - email lub imię z kontaktów
    pub to: Option<String>,
    /// Temat (wymagane)
    pub subject: Option<String>,
    /// Treść wiadomości (wymagane)
    pub body: Option<String>,
    /// CC
    #[serde(default)]
    pub cc: Vec<String>,
    /// Załączniki (ścieżki lub opisy)
    #[serde(default)]
    pub attachments: Vec<String>,
    /// Priorytet: "high", "normal", "low"
    pub priority: Option<String>,
}

impl EmailSendParams {
    pub fn missing_required(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.to.is_none() {
            missing.push("to");
        }
        if self.subject.is_none() {
            missing.push("subject");
        }
        if self.body.is_none() {
            missing.push("body");
        }
        missing
    }

    pub fn is_complete(&self) -> bool {
        self.to.is_some() && self.subject.is_some() && self.body.is_some()
    }
}

/// Parametry dla WebSearch
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct WebSearchParams {
    /// Zapytanie do wyszukania (wymagane)
    pub query: Option<String>,
    /// Typ wyszukiwania: "general", "news", "images", "videos"
    pub search_type: Option<String>,
    /// Język wyników
    pub language: Option<String>,
    /// Maksymalna liczba wyników
    pub max_results: Option<u32>,
}

impl WebSearchParams {
    pub fn missing_required(&self) -> Vec<&'static str> {
        if self.query.is_none() {
            vec!["query"]
        } else {
            vec![]
        }
    }

    pub fn is_complete(&self) -> bool {
        self.query.is_some()
    }
}

/// Parametry dla ReminderSet
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ReminderSetParams {
    /// O czym przypomnieć (wymagane)
    pub message: Option<String>,
    /// Kiedy przypomnieć (wymagane) - "za 30 minut", "jutro o 9:00"
    pub when: Option<String>,
    /// Powtarzanie: "codziennie", "co tydzień", "co miesiąc"
    pub repeat: Option<String>,
}

impl ReminderSetParams {
    pub fn missing_required(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.message.is_none() {
            missing.push("message");
        }
        if self.when.is_none() {
            missing.push("when");
        }
        missing
    }

    pub fn is_complete(&self) -> bool {
        self.message.is_some() && self.when.is_some()
    }
}

/// Parametry dla TimerSet
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TimerSetParams {
    /// Czas trwania (wymagane) - "5 minut", "1 godzina"
    pub duration: Option<String>,
    /// Nazwa/etykieta timera
    pub label: Option<String>,
}

impl TimerSetParams {
    pub fn missing_required(&self) -> Vec<&'static str> {
        if self.duration.is_none() {
            vec!["duration"]
        } else {
            vec![]
        }
    }

    pub fn is_complete(&self) -> bool {
        self.duration.is_some()
    }
}

/// Parametry dla NoteSave
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct NoteSaveParams {
    /// Treść notatki (wymagane)
    pub content: Option<String>,
    /// Tytuł notatki
    pub title: Option<String>,
    /// Tagi/kategorie
    #[serde(default)]
    pub tags: Vec<String>,
}

impl NoteSaveParams {
    pub fn missing_required(&self) -> Vec<&'static str> {
        if self.content.is_none() {
            vec!["content"]
        } else {
            vec![]
        }
    }

    pub fn is_complete(&self) -> bool {
        self.content.is_some()
    }
}

// ============================================================================
// MULTI-SPEAKER TYPES
// ============================================================================

/// Informacja o mówcy w multi-speaker scenario
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerInfo {
    /// Label mówcy (np. "Piotrek" lub "SPEAKER_01")
    pub label: String,
    /// Czy rozpoznany z bazy głosów
    pub is_known: bool,
    /// Similarity score (jeśli known)
    pub similarity: Option<f32>,
    /// Co powiedział ten mówca
    pub utterance: String,
    /// Wykryte intencje tego mówcy
    #[serde(default)]
    pub intents: Vec<Intent>,
}

/// Wynik analizy multi-speaker
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MultiSpeakerAnalysis {
    /// Liczba wykrytych mówców
    pub speaker_count: usize,
    /// Informacje o każdym mówcy
    #[serde(default)]
    pub speakers: Vec<SpeakerInfo>,
    /// Sugerowana odpowiedź (np. "zapytaj nieznanych o imię")
    pub suggested_response: Option<String>,
}

// ============================================================================
// ANALYSIS RESULT - pełny wynik analizy Bielika
// ============================================================================

/// Pełny wynik analizy intencji przez Bielika
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IntentAnalysisResult {
    /// Główna intencja (może być Conversation jeśli brak specjalnej)
    #[serde(default)]
    pub primary_intent: Option<Intent>,

    /// Dodatkowe intencje (jeśli wiele w jednej wypowiedzi)
    #[serde(default)]
    pub secondary_intents: Vec<Intent>,

    /// Wykryte wywołania narzędzi
    #[serde(default)]
    pub tool_calls: Vec<ToolCallResult>,

    /// Analiza multi-speaker (jeśli wykryto wielu mówców)
    #[serde(default)]
    pub multi_speaker: Option<MultiSpeakerAnalysis>,

    /// Czy wymaga zapytania do Memory
    #[serde(default)]
    pub needs_memory_query: bool,

    /// Terminy do wyszukania w Memory
    #[serde(default)]
    pub memory_search_terms: Vec<String>,

    /// Kontekst do wstrzyknięcia do głównego LLM
    #[serde(default)]
    pub context_for_llm: Option<String>,

    /// Uzasadnienie analizy (dla debugowania)
    #[serde(default)]
    pub reasoning: String,
}

/// Wynik pojedynczego wywołania narzędzia
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    /// ID wywołania (do śledzenia)
    pub call_id: String,

    /// Wywołanie narzędzia
    pub tool: ToolCall,

    /// Czy wszystkie wymagane parametry są dostępne
    pub is_complete: bool,

    /// Brakujące wymagane parametry
    #[serde(default)]
    pub missing_params: Vec<String>,

    /// Sugerowane pytanie do użytkownika (jeśli brakuje parametrów)
    pub follow_up_question: Option<String>,
}

impl ToolCallResult {
    /// Tworzy nowy ToolCallResult z automatycznym sprawdzeniem kompletności
    pub fn new(tool: ToolCall) -> Self {
        let call_id = uuid::Uuid::new_v4().to_string();
        let (is_complete, missing_params) = match &tool {
            ToolCall::CalendarAdd(p) => (
                p.is_complete(),
                p.missing_required().iter().map(|s| s.to_string()).collect(),
            ),
            ToolCall::CalendarCheck(_) => (true, vec![]),
            ToolCall::EmailSend(p) => (
                p.is_complete(),
                p.missing_required().iter().map(|s| s.to_string()).collect(),
            ),
            ToolCall::WebSearch(p) => (
                p.is_complete(),
                p.missing_required().iter().map(|s| s.to_string()).collect(),
            ),
            ToolCall::ReminderSet(p) => (
                p.is_complete(),
                p.missing_required().iter().map(|s| s.to_string()).collect(),
            ),
            ToolCall::TimerSet(p) => (
                p.is_complete(),
                p.missing_required().iter().map(|s| s.to_string()).collect(),
            ),
            ToolCall::NoteSave(p) => (
                p.is_complete(),
                p.missing_required().iter().map(|s| s.to_string()).collect(),
            ),
        };

        let follow_up_question = if !is_complete {
            Some(Self::generate_follow_up(&tool, &missing_params))
        } else {
            None
        };

        Self {
            call_id,
            tool,
            is_complete,
            missing_params,
            follow_up_question,
        }
    }

    /// Generuje pytanie uzupełniające dla brakujących parametrów
    fn generate_follow_up(tool: &ToolCall, missing: &[String]) -> String {
        match tool {
            ToolCall::CalendarAdd(_) => {
                if missing.iter().any(|s| s == "title") && missing.iter().any(|s| s == "date") {
                    "Co chcesz dodać do kalendarza i na kiedy?".to_string()
                } else if missing.iter().any(|s| s == "title") {
                    "Jak ma się nazywać to wydarzenie?".to_string()
                } else if missing.iter().any(|s| s == "date") {
                    "Na kiedy mam to zaplanować?".to_string()
                } else {
                    "Potrzebuję więcej szczegółów o wydarzeniu.".to_string()
                }
            }
            ToolCall::EmailSend(_) => {
                if missing.iter().any(|s| s == "to") {
                    "Do kogo mam wysłać tego maila?".to_string()
                } else if missing.iter().any(|s| s == "subject") {
                    "Jaki ma być temat wiadomości?".to_string()
                } else if missing.iter().any(|s| s == "body") {
                    "Co mam napisać w treści maila?".to_string()
                } else {
                    "Potrzebuję więcej informacji o mailu.".to_string()
                }
            }
            ToolCall::WebSearch(_) => "Czego mam poszukać w internecie?".to_string(),
            ToolCall::ReminderSet(_) => {
                if missing.iter().any(|s| s == "message") && missing.iter().any(|s| s == "when") {
                    "O czym mam Ci przypomnieć i kiedy?".to_string()
                } else if missing.iter().any(|s| s == "message") {
                    "O czym mam Ci przypomnieć?".to_string()
                } else {
                    "Kiedy mam Ci o tym przypomnieć?".to_string()
                }
            }
            ToolCall::TimerSet(_) => "Na ile czasu mam ustawić timer?".to_string(),
            ToolCall::NoteSave(_) => "Co mam zapisać w notatce?".to_string(),
            _ => "Potrzebuję więcej informacji.".to_string(),
        }
    }
}

// ============================================================================
// TOOL DEFINITION - definicje narzędzi dla C# klienta
// ============================================================================

/// Definicja narzędzia (dla C# klienta)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Nazwa narzędzia
    pub name: String,
    /// Opis co robi
    pub description: String,
    /// JSON Schema parametrów
    pub parameters_schema: serde_json::Value,
    /// Czy wymaga potwierdzenia użytkownika
    pub requires_confirmation: bool,
}

/// Lista dostępnych narzędzi
pub fn get_available_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "calendar_add".to_string(),
            description: "Dodaje wydarzenie do kalendarza".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "required": ["title", "date"],
                "properties": {
                    "title": { "type": "string", "description": "Tytuł wydarzenia" },
                    "date": { "type": "string", "description": "Data (YYYY-MM-DD lub 'jutro', 'w piątek')" },
                    "start_time": { "type": "string", "description": "Godzina rozpoczęcia (HH:MM)" },
                    "end_time": { "type": "string", "description": "Godzina zakończenia" },
                    "duration": { "type": "string", "description": "Czas trwania ('1h', '30min')" },
                    "location": { "type": "string", "description": "Lokalizacja" },
                    "description": { "type": "string", "description": "Opis wydarzenia" },
                    "attendees": { "type": "array", "items": { "type": "string" }, "description": "Lista uczestników" },
                    "reminder": { "type": "string", "description": "Przypomnienie ('15min', '1h', '1d')" }
                }
            }),
            requires_confirmation: false,
        },
        ToolDefinition {
            name: "calendar_check".to_string(),
            description: "Sprawdza kalendarz na dany dzień/okres".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "date": { "type": "string", "description": "Data do sprawdzenia" },
                    "date_range": { "type": "string", "description": "Zakres dat" },
                    "search_query": { "type": "string", "description": "Szukaj wydarzenia" }
                }
            }),
            requires_confirmation: false,
        },
        ToolDefinition {
            name: "email_send".to_string(),
            description: "Wysyła email".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "required": ["to", "subject", "body"],
                "properties": {
                    "to": { "type": "string", "description": "Adresat (email lub imię)" },
                    "subject": { "type": "string", "description": "Temat wiadomości" },
                    "body": { "type": "string", "description": "Treść wiadomości" },
                    "cc": { "type": "array", "items": { "type": "string" }, "description": "CC" },
                    "attachments": { "type": "array", "items": { "type": "string" }, "description": "Załączniki" },
                    "priority": { "type": "string", "enum": ["high", "normal", "low"], "description": "Priorytet" }
                }
            }),
            requires_confirmation: true,
        },
        ToolDefinition {
            name: "web_search".to_string(),
            description: "Przeszukuje internet".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "required": ["query"],
                "properties": {
                    "query": { "type": "string", "description": "Zapytanie do wyszukania" },
                    "search_type": { "type": "string", "enum": ["general", "news", "images", "videos"] },
                    "language": { "type": "string", "description": "Język wyników" },
                    "max_results": { "type": "integer", "description": "Max liczba wyników" }
                }
            }),
            requires_confirmation: false,
        },
        ToolDefinition {
            name: "reminder_set".to_string(),
            description: "Ustawia przypomnienie".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "required": ["message", "when"],
                "properties": {
                    "message": { "type": "string", "description": "O czym przypomnieć" },
                    "when": { "type": "string", "description": "Kiedy przypomnieć" },
                    "repeat": { "type": "string", "description": "Powtarzanie" }
                }
            }),
            requires_confirmation: false,
        },
        ToolDefinition {
            name: "timer_set".to_string(),
            description: "Ustawia timer/minutnik".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "required": ["duration"],
                "properties": {
                    "duration": { "type": "string", "description": "Czas trwania ('5 minut', '1h')" },
                    "label": { "type": "string", "description": "Nazwa timera" }
                }
            }),
            requires_confirmation: false,
        },
        ToolDefinition {
            name: "note_save".to_string(),
            description: "Zapisuje notatkę".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "required": ["content"],
                "properties": {
                    "content": { "type": "string", "description": "Treść notatki" },
                    "title": { "type": "string", "description": "Tytuł" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Tagi" }
                }
            }),
            requires_confirmation: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calendar_add_missing_params() {
        let params = CalendarAddParams::default();
        assert_eq!(params.missing_required(), vec!["title", "date"]);
        assert!(!params.is_complete());

        let params = CalendarAddParams {
            title: Some("Spotkanie".to_string()),
            date: Some("2024-01-15".to_string()),
            ..Default::default()
        };
        assert!(params.is_complete());
        assert!(params.missing_required().is_empty());
    }

    #[test]
    fn test_tool_call_result_generation() {
        let tool = ToolCall::CalendarAdd(CalendarAddParams {
            title: Some("Spotkanie".to_string()),
            date: None,
            ..Default::default()
        });

        let result = ToolCallResult::new(tool);
        assert!(!result.is_complete);
        assert_eq!(result.missing_params, vec!["date"]);
        assert!(result.follow_up_question.is_some());
    }

    #[test]
    fn test_intent_serialization() {
        let intent = Intent::Introduction {
            name: "Piotr".to_string(),
            confidence: 0.95,
        };

        let json = serde_json::to_string(&intent).unwrap();
        assert!(json.contains("introduction"));
        assert!(json.contains("Piotr"));
    }

    #[test]
    fn test_available_tools() {
        let tools = get_available_tools();
        assert_eq!(tools.len(), 7);
        assert!(tools.iter().any(|t| t.name == "calendar_add"));
        assert!(tools.iter().any(|t| t.name == "email_send"));
    }
}
