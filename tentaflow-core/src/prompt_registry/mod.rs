// ============================================================================
// PROMPT REGISTRY - Centralne zarządzanie promptami dla KV Cache
// ============================================================================
//
// CEL:
// Ten moduł przechowuje wszystkie stałe prompty systemowe w jednym miejscu
// z unikalnymi ID. Przy połączeniu z silnikiem LLM wysyłamy listę promptów
// do zacheowania jako KV (Key-Value) cache, co eliminuje potrzebę
// przeliczania attention dla tych samych prefixów.
//
// ARCHITEKTURA:
// - PromptRegistry: HashMap<PromptId, PromptEntry>
// - ModelPromptSet: Zestaw promptów dla konkretnego modelu
// - Dwa główne zestawy:
//   1. MAIN_LLM (bielik-11b): Jarvis assistant, personalizacja, session context
//   2. ANALYZER_LLM (bielik-1.5b): Query analysis, store analysis, disambiguation
//
// UŻYCIE:
// 1. Router/Desktop tworzy PromptRegistry przy starcie
// 2. Przy połączeniu z LLM Engine wysyła odpowiedni ModelPromptSet
// 3. LLM Engine cachuje KV dla tych promptów
// 4. Przy requestach używamy prompt_id zamiast pełnego tekstu
//
// ============================================================================

use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Unikalny identyfikator prompta
pub type PromptId = String;

/// Kategoria modelu dla którego prompt jest przeznaczony
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelCategory {
    /// Główny LLM (bielik-11b) - odpowiedzi użytkownikowi
    MainLlm,
    /// Analyzer LLM (bielik-1.5b) - analiza dla Memory, tools
    AnalyzerLlm,
}

impl std::fmt::Display for ModelCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelCategory::MainLlm => write!(f, "main_llm"),
            ModelCategory::AnalyzerLlm => write!(f, "analyzer_llm"),
        }
    }
}

impl ModelCategory {
    /// Konwertuje na typ protokołu dla wysyłki przez QUIC
    pub fn to_protocol(&self) -> tentaflow_protocol::PrefixCacheModelCategory {
        match self {
            ModelCategory::MainLlm => tentaflow_protocol::PrefixCacheModelCategory::MainLlm,
            ModelCategory::AnalyzerLlm => tentaflow_protocol::PrefixCacheModelCategory::AnalyzerLlm,
        }
    }
}

/// Typ prompta (jak ma być użyty)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptType {
    /// System prompt - pełny, stały
    System,
    /// Suffix - doklejany do system message
    Suffix,
    /// Template - wymaga formatowania z parametrami
    Template,
}

impl PromptType {
    /// Konwertuje na typ protokołu dla wysyłki przez QUIC
    pub fn to_protocol(&self) -> tentaflow_protocol::PrefixCachePromptType {
        match self {
            PromptType::System => tentaflow_protocol::PrefixCachePromptType::System,
            PromptType::Suffix => tentaflow_protocol::PrefixCachePromptType::Suffix,
            PromptType::Template => tentaflow_protocol::PrefixCachePromptType::Template,
        }
    }
}

/// Pojedynczy wpis w rejestrze promptów
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptEntry {
    /// Unikalny ID prompta
    pub id: PromptId,
    /// Kategoria modelu
    pub category: ModelCategory,
    /// Typ prompta
    pub prompt_type: PromptType,
    /// Opis (dla dokumentacji)
    pub description: String,
    /// Treść prompta
    pub content: String,
    /// Priorytet cachowania (wyższy = ważniejszy)
    pub cache_priority: u8,
}

impl PromptEntry {
    /// Konwertuje na typ protokołu dla wysyłki przez QUIC
    pub fn to_protocol(&self) -> tentaflow_protocol::PrefixCacheEntry {
        tentaflow_protocol::PrefixCacheEntry {
            id: self.id.clone(),
            category: self.category.to_protocol(),
            prompt_type: self.prompt_type.to_protocol(),
            content: self.content.clone(),
            cache_priority: self.cache_priority,
        }
    }
}

/// Zestaw promptów do wysłania do silnika LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPromptSet {
    /// Kategoria modelu
    pub category: ModelCategory,
    /// Lista promptów do zacheowania
    pub prompts: Vec<PromptEntry>,
}

/// Centralny rejestr wszystkich promptów
pub struct PromptRegistry {
    /// Mapa: PromptId → PromptEntry
    prompts: HashMap<PromptId, PromptEntry>,
    /// Indeks: ModelCategory → Vec<PromptId>
    by_category: HashMap<ModelCategory, Vec<PromptId>>,
}

impl PromptRegistry {
    /// Tworzy pusty rejestr - prompty ladowane wylacznie z bazy danych
    pub fn new() -> Self {
        Self {
            prompts: HashMap::new(),
            by_category: HashMap::new(),
        }
    }

    /// Rejestruje prompt
    pub fn register(&mut self, entry: PromptEntry) {
        let id = entry.id.clone();
        let category = entry.category;

        self.prompts.insert(id.clone(), entry);

        self.by_category
            .entry(category)
            .or_insert_with(Vec::new)
            .push(id);
    }

    /// Pobiera prompt po ID
    pub fn get(&self, id: &str) -> Option<&PromptEntry> {
        self.prompts.get(id)
    }

    /// Pobiera treść prompta po ID
    pub fn get_content(&self, id: &str) -> Option<&str> {
        self.prompts.get(id).map(|e| e.content.as_str())
    }

    /// Pobiera zestaw promptów dla danej kategorii modelu
    pub fn get_prompt_set(&self, category: ModelCategory) -> ModelPromptSet {
        let prompt_ids = self.by_category.get(&category).cloned().unwrap_or_default();

        let mut prompts: Vec<PromptEntry> = prompt_ids
            .iter()
            .filter_map(|id| self.prompts.get(id).cloned())
            .collect();

        // Sortuj po priorytecie (malejąco)
        prompts.sort_by(|a, b| b.cache_priority.cmp(&a.cache_priority));

        ModelPromptSet { category, prompts }
    }

    /// Zwraca wszystkie ID promptów
    pub fn all_ids(&self) -> Vec<&PromptId> {
        self.prompts.keys().collect()
    }

    /// Laduje prompty z bazy danych do rejestru
    pub fn load_from_db(&mut self, pool: &crate::db::DbPool) {
        let conn = match pool.lock() {
            Ok(c) => c,
            Err(e) => {
                warn!("Nie mozna uzyskac polaczenia z DB dla promptow: {}", e);
                return;
            }
        };

        let mut stmt = match conn.prepare(
            "SELECT prompt_id, content, prompt_type, cache_priority, default_model FROM prompts WHERE is_active = 1"
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!("Nie mozna odczytac promptow z DB: {}", e);
                return;
            }
        };

        let rows = match stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        }) {
            Ok(r) => r,
            Err(e) => {
                warn!("Blad zapytania promptow z DB: {}", e);
                return;
            }
        };

        let mut count = 0u32;
        for row in rows {
            let (prompt_id, content, prompt_type_str, cache_priority, default_model) = match row {
                Ok(r) => r,
                Err(_) => continue,
            };

            let prompt_type = match prompt_type_str.as_str() {
                "system" => PromptType::System,
                "suffix" => PromptType::Suffix,
                "template" => PromptType::Template,
                _ => PromptType::System,
            };

            let category = match default_model.as_deref() {
                Some("bielik-1.5b") => ModelCategory::AnalyzerLlm,
                _ => ModelCategory::MainLlm,
            };

            self.register(PromptEntry {
                id: prompt_id,
                category,
                prompt_type,
                description: String::new(),
                content,
                cache_priority: cache_priority.clamp(0, 255) as u8,
            });
            count += 1;
        }

        info!(
            "PromptRegistry: Zaladowano {} promptow z bazy danych",
            count
        );
    }

    /// Pobiera tresc prompta - panic jesli nie znaleziono (prompty MUSZA byc w DB)
    pub fn require_content(&self, id: &str) -> &str {
        self.prompts
            .get(id)
            .map(|e| e.content.as_str())
            .unwrap_or_else(|| {
                panic!(
                    "Brak wymaganego prompta '{}' w rejestrze. Sprawdz seed bazy danych.",
                    id
                )
            })
    }

    /// Formatuje template - panic jesli nie znaleziono
    pub fn require_template(&self, id: &str, params: &HashMap<&str, &str>) -> String {
        let entry = self.prompts.get(id).unwrap_or_else(|| {
            panic!(
                "Brak wymaganego template '{}' w rejestrze. Sprawdz seed bazy danych.",
                id
            )
        });

        let mut result = entry.content.clone();
        for (key, value) in params {
            result = result.replace(&format!("{{{}}}", key), value);
        }
        result
    }

    /// Formatuje template prompt z parametrami
    pub fn format_template(&self, id: &str, params: &HashMap<&str, &str>) -> Option<String> {
        let entry = self.prompts.get(id)?;

        if entry.prompt_type != PromptType::Template {
            return Some(entry.content.clone());
        }

        let mut result = entry.content.clone();
        for (key, value) in params {
            result = result.replace(&format!("{{{}}}", key), value);
        }

        Some(result)
    }
}

impl Default for PromptRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Thread-safe wrapper dla PromptRegistry
pub type SharedPromptRegistry = Arc<PromptRegistry>;

/// Tworzy wspoldzielony rejestr promptow z opcjonalnym ladowaniem z DB.
pub fn create_shared_registry(db_pool: Option<crate::db::DbPool>) -> SharedPromptRegistry {
    let mut registry = PromptRegistry::new();
    if let Some(ref pool) = db_pool {
        registry.load_from_db(pool);
    }
    Arc::new(registry)
}

// ============================================================================
// PROMPT IDs - Stałe dla wygody użycia w kodzie
// ============================================================================

/// ID promptów dla Main LLM
pub mod main_llm {
    pub const JARVIS_SYSTEM: &str = "jarvis_system";
    pub const SESSION_START: &str = "session_start";
    pub const SESSION_CONTINUE: &str = "session_continue";
    pub const SESSION_UNCLEAR: &str = "session_unclear";
    pub const UNKNOWN_USER: &str = "unknown_user";
    pub const UNKNOWN_USER_STRONG: &str = "unknown_user_strong";
    pub const PERSONALIZATION_TEMPLATE: &str = "personalization_template";
    pub const PERSONALIZATION_FIRST_TEMPLATE: &str = "personalization_first_template";
    pub const PERSONALIZATION_CONTINUE_TEMPLATE: &str = "personalization_continue_template";
    pub const MEMORY_CONTEXT_TEMPLATE: &str = "memory_context_template";
    pub const INTENT_ANALYZER_SYSTEM: &str = "intent_analyzer_system";
    pub const NEW_VOICE_DURING_CONVERSATION: &str = "new_voice_during_conversation";
    pub const NEW_SPEAKER_INTRODUCED_TEMPLATE: &str = "new_speaker_introduced_template";
    pub const MEDIUM_CONFIDENCE_KNOWN_TEMPLATE: &str = "medium_confidence_known_template";
    pub const MEDIUM_CONFIDENCE_UNKNOWN: &str = "medium_confidence_unknown";
}

/// ID promptów dla Analyzer LLM
pub mod analyzer_llm {
    pub const QUERY_ANALYSIS_SYSTEM: &str = "query_analysis_system";
    pub const STORE_ANALYSIS_SYSTEM: &str = "store_analysis_system";
    pub const DISAMBIGUATION_SYSTEM: &str = "disambiguation_system";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_registry() {
        let registry = PromptRegistry::new();
        assert!(registry.get(main_llm::JARVIS_SYSTEM).is_none());
    }

    #[test]
    fn test_register_and_get() {
        let mut registry = PromptRegistry::new();
        registry.register(PromptEntry {
            id: "test_prompt".to_string(),
            category: ModelCategory::MainLlm,
            prompt_type: PromptType::System,
            description: "Test".to_string(),
            content: "Testowa tresc".to_string(),
            cache_priority: 50,
        });
        assert_eq!(registry.require_content("test_prompt"), "Testowa tresc");
    }

    #[test]
    fn test_format_template() {
        let mut registry = PromptRegistry::new();
        registry.register(PromptEntry {
            id: "tmpl".to_string(),
            category: ModelCategory::MainLlm,
            prompt_type: PromptType::Template,
            description: "".to_string(),
            content: "Witaj {name}!".to_string(),
            cache_priority: 50,
        });
        let mut params = HashMap::new();
        params.insert("name", "Jan");
        assert_eq!(registry.require_template("tmpl", &params), "Witaj Jan!");
    }
}
