// =============================================================================
// Plik: addon/tool_dispatch.rs
// Opis: ToolDispatcher — integracja tool calling z LLM. Parsuje tool_calls
//       z odpowiedzi LLM, waliduje uprawnienia, deleguje wywolanie do addonu
//       WASM i formatuje wyniki w formacie OpenAI function calling.
// =============================================================================

use std::sync::Arc;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::AddonManager;

// =============================================================================
// Typy dla OpenAI tool calling
// =============================================================================

/// Pojedynczy tool_call z odpowiedzi LLM (format OpenAI)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmToolCall {
    /// Unikalny identyfikator wywolania
    pub id: String,
    /// Zawsze "function" w obecnym formacie OpenAI
    #[serde(rename = "type")]
    pub call_type: String,
    /// Dane funkcji do wywolania
    pub function: LlmFunctionCall,
}

/// Opis wywolania funkcji z LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmFunctionCall {
    /// Pelna nazwa narzedzia w formacie "addon_id.function_name"
    pub name: String,
    /// Argumenty jako JSON string (format OpenAI)
    pub arguments: String,
}

/// Wynik wywolania tool_call — do wstawienia do messages jako role=tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallResult {
    /// ID tool_call ktory zostal wywolany
    pub tool_call_id: String,
    /// Nazwa narzedzia
    pub name: String,
    /// Wynik w formacie JSON string
    pub content: String,
    /// Czy wywolanie zakonczone sukcesem
    pub success: bool,
}

// =============================================================================
// ToolDispatcher
// =============================================================================

/// Dispatcher tool calling — most miedzy LLM a addonami WASM.
/// Odpowiada za:
/// - Parsowanie tool_calls z odpowiedzi LLM
/// - Walidacje uprawnien uzytkownika
/// - Delegowanie wywolan do AddonManager
/// - Formatowanie wynikow w formacie OpenAI
pub struct ToolDispatcher {
    addon_manager: Arc<AddonManager>,
}

impl ToolDispatcher {
    /// Tworzy nowy ToolDispatcher z podanym AddonManager
    pub fn new(addon_manager: Arc<AddonManager>) -> Self {
        Self { addon_manager }
    }

    /// Wywoluje pojedynczy tool z addonu — sprawdza uprawnienia uzytkownika.
    ///
    /// Format tool_name: "addon_id.function_name" (np. "teams.send_message")
    /// Zwraca wynik jako JSON Value.
    pub fn dispatch_tool_call(
        &self,
        tool_name: &str,
        arguments: serde_json::Value,
        user_id: i64,
    ) -> Result<serde_json::Value> {
        // Parsuj addon_id i function_name z formatu "addon_id.function_name"
        let (addon_id, function_name) = tool_name.split_once('.')
            .ok_or_else(|| anyhow::anyhow!(
                "Niepoprawny format nazwy narzedzia: '{}'. Oczekiwany format: 'addon_id.function_name'",
                tool_name
            ))?;

        info!(
            "Dispatch tool call: addon='{}', function='{}', user_id={}",
            addon_id, function_name, user_id
        );

        // Sprawdz czy addon istnieje i ma zarejestrowane to narzedzie
        let tools = self.addon_manager.list_tools();
        let tool_exists = tools
            .iter()
            .any(|t| t.addon_id == addon_id && t.tool_name == function_name);

        if !tool_exists {
            bail!(
                "Narzedzie '{}.{}' nie jest zarejestrowane w zadnym addonanie",
                addon_id,
                function_name
            );
        }

        // Sprawdz uprawnienia uzytkownika do tego addonu
        let perm_result = self
            .addon_manager
            .permission_checker()
            .check(addon_id, user_id, "llm", None);

        if !perm_result.is_granted() {
            warn!(
                "Uzytkownik {} nie ma uprawnien do narzedzia '{}.{}'",
                user_id, addon_id, function_name
            );
            bail!(
                "Brak uprawnien do wywolania narzedzia '{}.{}'",
                addon_id,
                function_name
            );
        }

        // Deleguj wywolanie do AddonManager
        self.addon_manager
            .call_tool(addon_id, function_name, arguments, user_id)
    }

    /// Przetwarza liste tool_calls z odpowiedzi LLM.
    /// Wywoluje kazde narzedzie i zwraca wyniki.
    pub fn process_tool_calls(
        &self,
        tool_calls: &[LlmToolCall],
        user_id: i64,
    ) -> Vec<ToolCallResult> {
        tool_calls
            .iter()
            .map(|call| {
                let arguments: serde_json::Value = serde_json::from_str(&call.function.arguments)
                    .unwrap_or_else(|e| {
                        warn!(
                            "Niepoprawny JSON w argumentach tool_call '{}': {}",
                            call.function.name, e
                        );
                        serde_json::json!({})
                    });

                match self.dispatch_tool_call(&call.function.name, arguments, user_id) {
                    Ok(result) => ToolCallResult {
                        tool_call_id: call.id.clone(),
                        name: call.function.name.clone(),
                        content: serde_json::to_string(&result).unwrap_or_default(),
                        success: true,
                    },
                    Err(e) => {
                        warn!("Blad wywolania narzedzia '{}': {}", call.function.name, e);
                        ToolCallResult {
                            tool_call_id: call.id.clone(),
                            name: call.function.name.clone(),
                            content: serde_json::json!({
                                "error": e.to_string()
                            })
                            .to_string(),
                            success: false,
                        }
                    }
                }
            })
            .collect()
    }

    /// Zwraca liste narzedzi w formacie OpenAI function calling.
    /// Filtruje po uprawnieniach uzytkownika — zwraca tylko narzedzia
    /// do ktorych uzytkownik ma dostep.
    pub fn get_tools_for_llm(&self, user_id: i64) -> Vec<serde_json::Value> {
        self.addon_manager
            .list_tools()
            .into_iter()
            .filter(|tool| {
                // Sprawdz czy uzytkownik ma uprawnienie "llm" do tego addonu
                let result = self.addon_manager.permission_checker().check(
                    &tool.addon_id,
                    user_id,
                    "llm",
                    None,
                );
                result.is_granted()
            })
            .map(|tool| {
                // Format OpenAI function calling
                let full_name = format!("{}.{}", tool.addon_id, tool.tool_name);
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": full_name,
                        "description": tool.description,
                        "parameters": tool.parameters_schema,
                        "keywords": tool.keywords,
                    }
                })
            })
            .collect()
    }

    /// Zwraca liste narzedzi bez filtrowania uprawnien (dla admina/diagnostyki)
    pub fn get_all_tools_for_llm(&self) -> Vec<serde_json::Value> {
        self.addon_manager
            .list_tools()
            .into_iter()
            .map(|tool| {
                let full_name = format!("{}.{}", tool.addon_id, tool.tool_name);
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": full_name,
                        "description": tool.description,
                        "parameters": tool.parameters_schema,
                        "keywords": tool.keywords,
                    }
                })
            })
            .collect()
    }

    /// Formatuje wyniki tool_calls jako wiadomosci OpenAI (role=tool)
    pub fn format_results_as_messages(results: &[ToolCallResult]) -> Vec<serde_json::Value> {
        results
            .iter()
            .map(|result| {
                serde_json::json!({
                    "role": "tool",
                    "tool_call_id": result.tool_call_id,
                    "name": result.name,
                    "content": result.content,
                })
            })
            .collect()
    }

    /// Zwraca referencje do AddonManager
    pub fn addon_manager(&self) -> &Arc<AddonManager> {
        &self.addon_manager
    }
}
