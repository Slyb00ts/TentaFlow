// =============================================================================
// Plik: flow_engine/dispatchers_impl/prompts_impl.rs
// Opis: PromptsImpl — wrapper nad SharedPromptRegistry. Adapter widzi tylko
//       `get_prompt(key, locale)`, registry żyje dalej w ServiceManager bez
//       zmian. Locale jest dziś no-op (rejestr nie ma wariantów per locale);
//       gdy dochodzą warianty, suffix budowany w tym wrapperze.
// =============================================================================

use anyhow::Result;
use async_trait::async_trait;

use crate::flow_engine::dispatchers::PromptStore;
use crate::prompt_registry::SharedPromptRegistry;

pub struct PromptsImpl {
    registry: SharedPromptRegistry,
}

impl PromptsImpl {
    pub fn new(registry: SharedPromptRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl PromptStore for PromptsImpl {
    async fn get_prompt(&self, key: &str, _locale: Option<&str>) -> Result<Option<String>> {
        Ok(self.registry.get_content(key).map(|s| s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt_registry::{ModelCategory, PromptEntry, PromptRegistry, PromptType};
    use std::sync::Arc;

    fn registry_with(id: &str, content: &str) -> SharedPromptRegistry {
        let mut r = PromptRegistry::new();
        r.register(PromptEntry {
            id: id.to_string(),
            category: ModelCategory::MainLlm,
            prompt_type: PromptType::System,
            description: "test".into(),
            content: content.into(),
            cache_priority: 0,
        });
        Arc::new(r)
    }

    #[tokio::test]
    async fn returns_content_when_present() {
        let p = PromptsImpl::new(registry_with("greet", "hello"));
        assert_eq!(p.get_prompt("greet", None).await.unwrap(), Some("hello".into()));
    }

    #[tokio::test]
    async fn returns_none_when_missing() {
        let p = PromptsImpl::new(registry_with("greet", "hi"));
        assert_eq!(p.get_prompt("missing", Some("pl")).await.unwrap(), None);
    }
}
