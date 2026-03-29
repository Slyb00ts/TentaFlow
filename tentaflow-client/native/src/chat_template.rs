// ============================================================================
// CHAT TEMPLATES - Predefiniowane formaty promptów dla różnych modeli LLM
// ============================================================================
//
// CEL:
// Umożliwia formatowanie messages do promptu zgodnie z wymaganiami modelu.
// Obsługuje predefiniowane szablony (Llama3, ChatML, Alpaca, Vicuna) oraz custom.
//
// UŻYCIE:
// ```rust
// let template = ChatTemplate::Llama3;
// let prompt = template.format(&messages);
// ```
//
// ============================================================================

/// Predefiniowane i custom chat templates dla modeli LLM.
#[derive(Debug, Clone)]
pub enum ChatTemplate {
    /// Automatyczny - serwer użyje template z modelu (vLLM tokenizer).
    /// Nie formatuje promptu, wysyła messages do serwera.
    Auto,

    /// Llama 3 Instruct format (używany przez Bielik, Llama 3, Llama 3.1).
    /// ```
    /// <|start_header_id|>system<|end_header_id|>
    /// {system}<|eot_id|>
    /// <|start_header_id|>user<|end_header_id|>
    /// {user}<|eot_id|>
    /// <|start_header_id|>assistant<|end_header_id|>
    /// ```
    Llama3,

    /// ChatML format (używany przez Qwen, OpenChat, niektóre fine-tuned modele).
    /// ```
    /// <|im_start|>system
    /// {system}<|im_end|>
    /// <|im_start|>user
    /// {user}<|im_end|>
    /// <|im_start|>assistant
    /// ```
    ChatML,

    /// Alpaca format (używany przez Stanford Alpaca i pochodne).
    /// ```
    /// ### Instruction:
    /// {system}
    ///
    /// ### Input:
    /// {user}
    ///
    /// ### Response:
    /// ```
    Alpaca,

    /// Vicuna format (używany przez Vicuna, FastChat modele).
    /// ```
    /// SYSTEM: {system}
    /// USER: {user}
    /// ASSISTANT:
    /// ```
    Vicuna,

    /// Mistral Instruct format.
    /// ```
    /// <s>[INST] {system}
    ///
    /// {user} [/INST]
    /// ```
    Mistral,

    /// Custom template z pełną kontrolą nad formatem.
    Custom {
        /// Format wiadomości systemowej. {{content}} zostanie zastąpione treścią.
        system_format: String,
        /// Format wiadomości użytkownika. {{content}} zostanie zastąpione treścią.
        user_format: String,
        /// Format wiadomości asystenta. {{content}} zostanie zastąpione treścią.
        assistant_format: String,
        /// Prefix przed odpowiedzią asystenta (dodawany na końcu).
        assistant_prefix: String,
        /// Tokeny stop (opcjonalne - zwracane dla informacji).
        stop_tokens: Vec<String>,
    },
}

impl Default for ChatTemplate {
    fn default() -> Self {
        ChatTemplate::Auto
    }
}

impl ChatTemplate {
    /// Formatuje messages do pojedynczego promptu.
    /// Zwraca None dla ChatTemplate::Auto (serwer ma użyć template modelu).
    pub fn format(&self, messages: &[(String, String)]) -> Option<String> {
        match self {
            ChatTemplate::Auto => None,
            ChatTemplate::Llama3 => Some(self.format_llama3(messages)),
            ChatTemplate::ChatML => Some(self.format_chatml(messages)),
            ChatTemplate::Alpaca => Some(self.format_alpaca(messages)),
            ChatTemplate::Vicuna => Some(self.format_vicuna(messages)),
            ChatTemplate::Mistral => Some(self.format_mistral(messages)),
            ChatTemplate::Custom {
                system_format,
                user_format,
                assistant_format,
                assistant_prefix,
                ..
            } => Some(self.format_custom(messages, system_format, user_format, assistant_format, assistant_prefix)),
        }
    }

    /// Zwraca stop tokens dla tego template.
    pub fn stop_tokens(&self) -> Vec<String> {
        match self {
            ChatTemplate::Auto => vec![],
            ChatTemplate::Llama3 => vec![
                "<|eot_id|>".to_string(),
                "<|start_header_id|>".to_string(),
                "<|end_header_id|>".to_string(),
            ],
            ChatTemplate::ChatML => vec![
                "<|im_end|>".to_string(),
                "<|im_start|>".to_string(),
            ],
            ChatTemplate::Alpaca => vec![
                "### ".to_string(),
            ],
            ChatTemplate::Vicuna => vec![
                "USER:".to_string(),
                "SYSTEM:".to_string(),
            ],
            ChatTemplate::Mistral => vec![
                "</s>".to_string(),
                "[INST]".to_string(),
            ],
            ChatTemplate::Custom { stop_tokens, .. } => stop_tokens.clone(),
        }
    }

    // =========================================================================
    // PRIVATE FORMATTERS
    // =========================================================================

    fn format_llama3(&self, messages: &[(String, String)]) -> String {
        // BOS token na początku zgodnie z Llama 3 Instruct format
        let mut prompt = String::from("<s>");

        for (role, content) in messages {
            let role_name = match role.as_str() {
                "system" => "system",
                "user" => "user",
                "assistant" => "assistant",
                other => other,
            };

            prompt.push_str(&format!(
                "<|start_header_id|>{}<|end_header_id|>\n\n{}<|eot_id|>",
                role_name, content
            ));
        }

        // Dodaj początek odpowiedzi asystenta
        prompt.push_str("<|start_header_id|>assistant<|end_header_id|>\n\n");
        prompt
    }

    fn format_chatml(&self, messages: &[(String, String)]) -> String {
        // Bielik-11B ChatML format: <s><|im_start|> role\ncontent<|im_end|> \n
        let mut prompt = String::from("<s>");

        for (role, content) in messages {
            let role_name = match role.as_str() {
                "system" => "system",
                "user" => "user",
                "assistant" => "assistant",
                other => other,
            };

            prompt.push_str(&format!(
                "<|im_start|> {}\n{}<|im_end|> \n",
                role_name, content
            ));
        }

        prompt.push_str("<|im_start|> assistant\n");
        prompt
    }

    fn format_alpaca(&self, messages: &[(String, String)]) -> String {
        let mut prompt = String::new();
        let mut system_msg = String::new();
        let mut user_msg = String::new();

        for (role, content) in messages {
            match role.as_str() {
                "system" => system_msg = content.clone(),
                "user" => user_msg = content.clone(),
                "assistant" => {
                    // Dla kontekstu - poprzednia odpowiedź asystenta
                    prompt.push_str(&format!("### Response:\n{}\n\n", content));
                }
                _ => {}
            }
        }

        if !system_msg.is_empty() {
            prompt.push_str(&format!("### Instruction:\n{}\n\n", system_msg));
        }

        if !user_msg.is_empty() {
            prompt.push_str(&format!("### Input:\n{}\n\n", user_msg));
        }

        prompt.push_str("### Response:\n");
        prompt
    }

    fn format_vicuna(&self, messages: &[(String, String)]) -> String {
        let mut prompt = String::new();

        for (role, content) in messages {
            match role.as_str() {
                "system" => prompt.push_str(&format!("SYSTEM: {}\n", content)),
                "user" => prompt.push_str(&format!("USER: {}\n", content)),
                "assistant" => prompt.push_str(&format!("ASSISTANT: {}\n", content)),
                _ => {}
            }
        }

        prompt.push_str("ASSISTANT: ");
        prompt
    }

    fn format_mistral(&self, messages: &[(String, String)]) -> String {
        let mut prompt = String::from("<s>");
        let mut system_content = String::new();

        for (role, content) in messages {
            match role.as_str() {
                "system" => system_content = content.clone(),
                "user" => {
                    prompt.push_str("[INST] ");
                    if !system_content.is_empty() {
                        prompt.push_str(&system_content);
                        prompt.push_str("\n\n");
                        system_content.clear();
                    }
                    prompt.push_str(content);
                    prompt.push_str(" [/INST]");
                }
                "assistant" => {
                    prompt.push_str(content);
                    prompt.push_str("</s>");
                }
                _ => {}
            }
        }

        prompt
    }

    fn format_custom(
        &self,
        messages: &[(String, String)],
        system_format: &str,
        user_format: &str,
        assistant_format: &str,
        assistant_prefix: &str,
    ) -> String {
        let mut prompt = String::new();

        for (role, content) in messages {
            let format_str = match role.as_str() {
                "system" => system_format,
                "user" => user_format,
                "assistant" => assistant_format,
                _ => continue,
            };

            let formatted = format_str.replace("{{content}}", content);
            prompt.push_str(&formatted);
        }

        prompt.push_str(assistant_prefix);
        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_llama3_format() {
        let messages = vec![
            ("system".to_string(), "You are a helpful assistant.".to_string()),
            ("user".to_string(), "Hello!".to_string()),
        ];

        let template = ChatTemplate::Llama3;
        let prompt = template.format(&messages).unwrap();

        // Llama 3 Instruct format z BOS tokenem na początku
        assert!(prompt.starts_with("<s>"));
        assert!(prompt.contains("<|start_header_id|>system<|end_header_id|>"));
        assert!(prompt.contains("You are a helpful assistant."));
        assert!(prompt.contains("<|eot_id|>"));
        assert!(prompt.ends_with("<|start_header_id|>assistant<|end_header_id|>\n\n"));
    }

    #[test]
    fn test_chatml_format() {
        let messages = vec![
            ("user".to_string(), "Hello!".to_string()),
        ];

        let template = ChatTemplate::ChatML;
        let prompt = template.format(&messages).unwrap();

        assert!(prompt.contains("<|im_start|>user"));
        assert!(prompt.contains("<|im_end|>"));
        assert!(prompt.ends_with("<|im_start|>assistant\n"));
    }

    #[test]
    fn test_auto_returns_none() {
        let messages = vec![
            ("user".to_string(), "Hello!".to_string()),
        ];

        let template = ChatTemplate::Auto;
        assert!(template.format(&messages).is_none());
    }

    #[test]
    fn test_custom_template() {
        let messages = vec![
            ("system".to_string(), "Be helpful.".to_string()),
            ("user".to_string(), "Hi!".to_string()),
        ];

        let template = ChatTemplate::Custom {
            system_format: "[SYS]{{content}}[/SYS]".to_string(),
            user_format: "[USR]{{content}}[/USR]".to_string(),
            assistant_format: "[AST]{{content}}[/AST]".to_string(),
            assistant_prefix: "[AST]".to_string(),
            stop_tokens: vec!["[/AST]".to_string()],
        };

        let prompt = template.format(&messages).unwrap();
        assert!(prompt.contains("[SYS]Be helpful.[/SYS]"));
        assert!(prompt.contains("[USR]Hi![/USR]"));
        assert!(prompt.ends_with("[AST]"));
    }
}
