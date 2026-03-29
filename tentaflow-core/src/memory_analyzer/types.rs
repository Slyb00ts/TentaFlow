// =============================================================================
// Plik: memory_analyzer/types.rs
// Opis: Typy analizatora pamieci — MemoryQueryType, QueryDecision, StoreDecision,
//       ExtractedEntity, ExtractedRelation, ExtractedFact, MemoryContext.
// =============================================================================

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Typ zapytania do Memory (rozszerzenie QueryType z RAG)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MemoryQueryType {
    /// Nowy temat - wymaga wyszukiwania w Memory
    NewSearch,
    /// Uszczegółowienie - użyj poprzedniego kontekstu
    Refine,
    /// Rozszerzenie tematu - połącz z poprzednim kontekstem
    Expand,
    /// Nie wymaga zapytania do Memory
    None,
}

impl Default for MemoryQueryType {
    fn default() -> Self {
        Self::None
    }
}

/// Filtr czasowy dla zapytań do Memory
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TimeFilter {
    /// Ostatnie interakcje (domyślnie)
    Recent,
    /// Wszystkie dane
    All,
    /// Konkretna data lub zakres
    Specific(String),
}

impl Default for TimeFilter {
    fn default() -> Self {
        Self::Recent
    }
}

/// Decyzja o zapytaniu do Memory (przed wywołaniem głównego modelu)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct QueryDecision {
    /// Czy powinniśmy odpytać Memory
    pub should_query: bool,
    /// Typ zapytania
    pub query_type: MemoryQueryType,
    /// Terminy do wyszukania (encje, słowa kluczowe)
    #[serde(default)]
    pub search_terms: Vec<String>,
    /// Typy relacji do wyszukania
    #[serde(default)]
    pub relation_types: Vec<String>,
    /// Filtr czasowy
    #[serde(default)]
    pub time_filter: TimeFilter,
    /// Uzasadnienie decyzji (dla debugowania)
    #[serde(default)]
    pub reasoning: String,
}

/// Typ encji w Memory
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub enum EntityType {
    Person,
    Project,
    Organization,
    Location,
    Event,
    Concept,
    Document,
    Product,
    Technology,
    Profession,
    Skill,
    #[serde(other)]
    Other,
}

impl Default for EntityType {
    fn default() -> Self {
        Self::Other
    }
}

/// Encja wyekstrahowana z rozmowy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedEntity {
    /// Nazwa encji
    pub name: String,
    /// Typ encji
    #[serde(default)]
    pub entity_type: EntityType,
    /// Dodatkowe atrybuty (wartości konwertowane do String)
    #[serde(default, deserialize_with = "deserialize_string_map")]
    pub attributes: HashMap<String, String>,
    /// Pewność ekstrakcji (0.0-1.0)
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

/// Relacja wyekstrahowana z rozmowy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedRelation {
    /// Encja źródłowa (nazwa)
    pub from: String,
    /// Encja docelowa (nazwa)
    pub to: String,
    /// Typ relacji (IsA, WorksOn, Met, LocatedIn, etc.)
    pub relation_type: String,
    /// Dodatkowe metadane (kiedy, gdzie, kontekst)
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, String>,
    /// Pewność ekstrakcji (0.0-1.0)
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

/// Fakt wyekstrahowany z rozmowy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFact {
    /// Treść faktu (zdanie)
    pub text: String,
    /// Referencje do encji (nazwy)
    #[serde(default)]
    pub entity_refs: Vec<String>,
    /// Pewność ekstrakcji (0.0-1.0)
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

/// Decyzja o zapisie do Memory (po odpowiedzi modelu)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoreDecision {
    /// Czy powinniśmy zapisać do Memory
    pub should_store: bool,
    /// Ważność informacji (0.0-1.0) - wpływa na priorytet konsolidacji
    #[serde(default)]
    pub importance: f32,
    /// Wyekstrahowane encje
    #[serde(default)]
    pub entities: Vec<ExtractedEntity>,
    /// Wyekstrahowane relacje
    #[serde(default)]
    pub relations: Vec<ExtractedRelation>,
    /// Wyekstrahowane fakty
    #[serde(default)]
    pub facts: Vec<ExtractedFact>,
    /// Uzasadnienie decyzji (dla debugowania)
    #[serde(default)]
    pub reasoning: String,
}

/// Informacja o niejednoznaczności wymagającej wyjaśnienia
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DisambiguationInfo {
    /// Czy wymagane jest wyjaśnienie
    pub needed: bool,
    /// Niejednoznaczna encja
    #[serde(default)]
    pub ambiguous_entity: Option<String>,
    /// Kandydaci do wyboru
    #[serde(default)]
    pub candidates: Vec<DisambiguationCandidate>,
}

/// Kandydat w disambiguation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisambiguationCandidate {
    /// ID węzła w Memory
    pub node_id: u64,
    /// Nazwa/opis
    pub name: String,
    /// Kontekst (np. "kolega z pracy", "klient")
    #[serde(default)]
    pub context: String,
    /// Score dopasowania
    #[serde(default)]
    pub score: f32,
}

/// Pełny wynik analizy Memory Analyzer
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryAnalysisResult {
    /// Decyzja o zapytaniu (przed modelem)
    pub query_decision: QueryDecision,
    /// Decyzja o zapisie (po modelu) - wypełniana w drugiej fazie
    #[serde(default)]
    pub store_decision: Option<StoreDecision>,
    /// Informacja o disambiguation
    #[serde(default)]
    pub disambiguation: DisambiguationInfo,
}

/// Kontekst z Memory przekazywany do głównego modelu
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryContext {
    /// Znalezione węzły (jako tekst)
    #[serde(default)]
    pub nodes: Vec<MemoryNodeInfo>,
    /// Znalezione relacje (jako tekst)
    #[serde(default)]
    pub relations: Vec<MemoryRelationInfo>,
    /// Powiązane fakty
    #[serde(default)]
    pub facts: Vec<String>,
    /// Sformatowany kontekst do wstawienia do prompta
    #[serde(default)]
    pub formatted_context: String,
}

/// Informacja o węźle z Memory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryNodeInfo {
    pub id: u64,
    pub name: String,
    pub node_type: String,
    #[serde(default)]
    pub attributes: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub last_accessed: Option<String>,
}

/// Informacja o relacji z Memory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRelationInfo {
    pub from_name: String,
    pub to_name: String,
    pub relation_type: String,
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, String>,
}

fn default_confidence() -> f32 {
    0.8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_decision_default() {
        let decision = QueryDecision::default();
        assert!(!decision.should_query);
        assert_eq!(decision.query_type, MemoryQueryType::None);
    }

    #[test]
    fn test_store_decision_serialization() {
        let decision = StoreDecision {
            should_store: true,
            importance: 0.8,
            entities: vec![ExtractedEntity {
                name: "Marek".to_string(),
                entity_type: EntityType::Person,
                attributes: HashMap::new(),
                confidence: 0.9,
            }],
            relations: vec![],
            facts: vec![],
            reasoning: "Test".to_string(),
        };

        let json = serde_json::to_string(&decision).unwrap();
        assert!(json.contains("Marek"));
        assert!(json.contains("Person"));
    }

    #[test]
    fn test_attributes_with_int_values() {
        let json = r#"{"name": "test", "entity_type": "Person", "attributes": {"age": 42, "name": "Jan"}, "confidence": 0.9}"#;
        let entity: ExtractedEntity = serde_json::from_str(json).unwrap();
        assert_eq!(entity.attributes.get("age"), Some(&"42".to_string()));
        assert_eq!(entity.attributes.get("name"), Some(&"Jan".to_string()));
    }
}

/// Deserializuje HashMap akceptując dowolne typy wartości i konwertując do String
fn deserialize_string_map<'de, D>(deserializer: D) -> Result<HashMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::{MapAccess, Visitor};

    struct StringMapVisitor;

    impl<'de> Visitor<'de> for StringMapVisitor {
        type Value = HashMap<String, String>;

        fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
            formatter.write_str("a map with string keys and any values")
        }

        fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            let mut map = HashMap::with_capacity(access.size_hint().unwrap_or(0));

            while let Some((key, value)) = access.next_entry::<String, AnyValue>()? {
                map.insert(key, value.0);
            }

            Ok(map)
        }
    }

    deserializer.deserialize_map(StringMapVisitor)
}

/// Wrapper do deserializacji dowolnej wartości jako String
struct AnyValue(String);

impl<'de> Deserialize<'de> for AnyValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::{self, Visitor};

        struct AnyValueVisitor;

        impl<'de> Visitor<'de> for AnyValueVisitor {
            type Value = AnyValue;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("any value")
            }

            fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
                Ok(AnyValue(v.to_string()))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
                Ok(AnyValue(v.to_string()))
            }

            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
                Ok(AnyValue(v.to_string()))
            }

            fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E> {
                Ok(AnyValue(v.to_string()))
            }

            fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(AnyValue(v.to_string()))
            }

            fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
                Ok(AnyValue(v))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(AnyValue(String::new()))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(AnyValue(String::new()))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element::<serde_json::Value>()? {
                    items.push(item.to_string());
                }
                Ok(AnyValue(items.join(", ")))
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                use serde::de::value::MapAccessDeserializer;
                let value: serde_json::Value = Deserialize::deserialize(MapAccessDeserializer::new(map))?;
                Ok(AnyValue(value.to_string()))
            }
        }

        deserializer.deserialize_any(AnyValueVisitor)
    }
}
