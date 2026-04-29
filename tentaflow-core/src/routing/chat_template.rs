// =============================================================================
// Plik: routing/chat_template.rs
// Opis: System szablonow chatu — auto-detekcja i formatowanie promptow
//       dla roznych architektur modeli (ChatML, Llama3, Mistral, Plain).
// =============================================================================

use std::path::Path;
use tracing::{debug, warn};

/// Szablon formatowania chatu — odpowiada architekturze modelu
#[derive(Debug, Clone, PartialEq)]
pub enum ChatTemplate {
    /// ChatML (Qwen, Bielik, Yi, OpenHermes) — <|im_start|>/<|im_end|>
    ChatML,
    /// Llama 3 — <|begin_of_text|>, <|start_header_id|>/<|end_header_id|>
    Llama3,
    /// Mistral / Mixtral — [INST]/[/INST]
    Mistral,
    /// Alpaca — ### Instruction / ### Input / ### Response
    Alpaca,
    /// Fallback — prosty format tekstowy bez specjalnych tokenow
    Plain,
}

/// Pojedyncza wiadomosc w konwersacji
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// Rola: "system", "user", "assistant"
    pub role: String,
    /// Tresc wiadomosci
    pub content: String,
}

impl ChatTemplate {
    /// Zwraca nazwe szablonu jako string (do serializacji/logowania)
    pub fn name(&self) -> &str {
        match self {
            ChatTemplate::ChatML => "chatml",
            ChatTemplate::Llama3 => "llama3",
            ChatTemplate::Mistral => "mistral",
            ChatTemplate::Alpaca => "alpaca",
            ChatTemplate::Plain => "plain",
        }
    }

    /// Formatuje liste wiadomosci wedlug szablonu modelu.
    /// `add_generation_prompt` — czy dodac poczatek odpowiedzi asystenta na koncu
    pub fn format_messages(&self, messages: &[ChatMessage], add_generation_prompt: bool) -> String {
        match self {
            ChatTemplate::ChatML => Self::format_chatml(messages, add_generation_prompt),
            ChatTemplate::Llama3 => Self::format_llama3(messages, add_generation_prompt),
            ChatTemplate::Mistral => Self::format_mistral(messages, add_generation_prompt),
            ChatTemplate::Alpaca => Self::format_alpaca(messages, add_generation_prompt),
            ChatTemplate::Plain => Self::format_plain(messages, add_generation_prompt),
        }
    }

    /// Zwraca stop tokeny specyficzne dla szablonu
    pub fn stop_tokens(&self) -> Vec<String> {
        match self {
            // Pelny zestaw stop tokenow dla ChatML (zgodnie z mlx-swift na iOS,
            // gdzie Bielik 4.5B v3.0 dziala bez bełkotu). Sam <|im_end|> nie wystarczy,
            // bo niektore quantized modele wpadaja w <|endoftext|>/<s>/</s> i nigdy
            // nie konczyly.
            ChatTemplate::ChatML => vec![
                "<|im_end|>".to_string(),
                "<|endoftext|>".to_string(),
                "</s>".to_string(),
            ],
            ChatTemplate::Llama3 => vec!["<|eot_id|>".to_string()],
            ChatTemplate::Mistral => vec!["[/INST]".to_string()],
            ChatTemplate::Alpaca => vec!["### Instruction:".to_string(), "### Input:".to_string()],
            ChatTemplate::Plain => vec![],
        }
    }

    // ========================================================================
    // FORMATY SZABLONOW
    // ========================================================================

    /// ChatML — uzywany przez Qwen, Bielik, Yi, OpenHermes
    fn format_chatml(messages: &[ChatMessage], add_generation_prompt: bool) -> String {
        let mut output = String::new();

        for msg in messages {
            output.push_str("<|im_start|>");
            output.push_str(&msg.role);
            output.push('\n');
            output.push_str(&msg.content);
            output.push_str("<|im_end|>\n");
        }

        if add_generation_prompt {
            output.push_str("<|im_start|>assistant\n");
        }

        output
    }

    /// Llama 3 — uzywany przez Meta Llama 3.x
    fn format_llama3(messages: &[ChatMessage], add_generation_prompt: bool) -> String {
        let mut output = String::from("<|begin_of_text|>");

        for msg in messages {
            output.push_str("<|start_header_id|>");
            output.push_str(&msg.role);
            output.push_str("<|end_header_id|>\n\n");
            output.push_str(&msg.content);
            output.push_str("<|eot_id|>");
        }

        if add_generation_prompt {
            output.push_str("<|start_header_id|>assistant<|end_header_id|>\n\n");
        }

        output
    }

    /// Mistral / Mixtral — format [INST]
    fn format_mistral(messages: &[ChatMessage], _add_generation_prompt: bool) -> String {
        let mut output = String::new();
        let mut system_text = String::new();

        // Zbierz system prompt
        for msg in messages {
            if msg.role == "system" {
                system_text = msg.content.clone();
            }
        }

        // Buduj konwersacje w parach user/assistant
        let mut in_inst = false;
        for msg in messages {
            match msg.role.as_str() {
                "system" => continue,
                "user" => {
                    output.push_str("[INST] ");
                    if !system_text.is_empty() {
                        output.push_str(&system_text);
                        output.push_str("\n\n");
                        // System prompt dodajemy tylko raz
                        system_text.clear();
                    }
                    output.push_str(&msg.content);
                    output.push_str(" [/INST]");
                    in_inst = true;
                }
                "assistant" => {
                    if in_inst {
                        output.push(' ');
                    }
                    output.push_str(&msg.content);
                    in_inst = false;
                }
                _ => {}
            }
        }

        output
    }

    /// Alpaca — prosty format instrukcji
    fn format_alpaca(messages: &[ChatMessage], add_generation_prompt: bool) -> String {
        let mut system_text = String::new();
        let mut user_text = String::new();

        for msg in messages {
            match msg.role.as_str() {
                "system" => system_text = msg.content.clone(),
                "user" => user_text = msg.content.clone(),
                _ => {}
            }
        }

        let mut output = String::new();

        if !system_text.is_empty() {
            output.push_str("### Instruction:\n");
            output.push_str(&system_text);
            output.push_str("\n\n");
        }

        if !user_text.is_empty() {
            output.push_str("### Input:\n");
            output.push_str(&user_text);
            output.push_str("\n\n");
        }

        if add_generation_prompt {
            output.push_str("### Response:\n");
        }

        output
    }

    /// Plain — fallback bez specjalnych tokenow
    fn format_plain(messages: &[ChatMessage], add_generation_prompt: bool) -> String {
        let mut output = String::new();

        for msg in messages {
            match msg.role.as_str() {
                "system" => {
                    output.push_str("System: ");
                    output.push_str(&msg.content);
                    output.push_str("\n\n");
                }
                "user" => {
                    output.push_str("User: ");
                    output.push_str(&msg.content);
                    output.push('\n');
                }
                "assistant" => {
                    output.push_str("Assistant: ");
                    output.push_str(&msg.content);
                    output.push('\n');
                }
                other => {
                    output.push_str(other);
                    output.push_str(": ");
                    output.push_str(&msg.content);
                    output.push('\n');
                }
            }
        }

        if add_generation_prompt {
            output.push_str("Assistant: ");
        }

        output
    }
}

/// Auto-detekcja szablonu chatu na podstawie plikow konfiguracyjnych modelu.
/// Kolejnosc sprawdzania:
/// 1. tokenizer_config.json -> pole "chat_template" (Jinja2)
/// 2. tokenizer_config.json -> pole "added_tokens_decoder" (specjalne tokeny)
/// 3. Fallback: Plain
pub fn detect_chat_template(model_dir: &Path) -> ChatTemplate {
    let config_path = model_dir.join("tokenizer_config.json");

    // Proba wczytania tokenizer_config.json
    let config_content = match std::fs::read_to_string(&config_path) {
        Ok(content) => content,
        Err(e) => {
            warn!(
                "Nie udalo sie wczytac tokenizer_config.json z {}: {} — uzywam Plain",
                model_dir.display(),
                e,
            );
            return ChatTemplate::Plain;
        }
    };

    let config: serde_json::Value = match serde_json::from_str(&config_content) {
        Ok(v) => v,
        Err(e) => {
            warn!(
                "Blad parsowania tokenizer_config.json: {} — uzywam Plain",
                e
            );
            return ChatTemplate::Plain;
        }
    };

    // Krok 1: Sprawdz pole "chat_template" (string Jinja2)
    if let Some(template_str) = config.get("chat_template").and_then(|v| v.as_str()) {
        debug!("Znaleziono pole chat_template w tokenizer_config.json");

        if template_str.contains("im_start") {
            debug!("Wykryto szablon ChatML (im_start w chat_template)");
            return ChatTemplate::ChatML;
        }

        if template_str.contains("start_header_id") {
            debug!("Wykryto szablon Llama3 (start_header_id w chat_template)");
            return ChatTemplate::Llama3;
        }

        if template_str.contains("[INST]") {
            debug!("Wykryto szablon Mistral ([INST] w chat_template)");
            return ChatTemplate::Mistral;
        }

        if template_str.contains("### Instruction") {
            debug!("Wykryto szablon Alpaca (### Instruction w chat_template)");
            return ChatTemplate::Alpaca;
        }
    }

    // Krok 2: Sprawdz added_tokens_decoder — szukaj specjalnych tokenow
    if let Some(added_tokens) = config.get("added_tokens_decoder") {
        let tokens_str = added_tokens.to_string();

        if tokens_str.contains("im_start") || tokens_str.contains("im_end") {
            debug!("Wykryto szablon ChatML (im_start/im_end w added_tokens_decoder)");
            return ChatTemplate::ChatML;
        }

        if tokens_str.contains("start_header_id") || tokens_str.contains("end_header_id") {
            debug!("Wykryto szablon Llama3 (start_header_id w added_tokens_decoder)");
            return ChatTemplate::Llama3;
        }
    }

    // Krok 3: Sprawdz eos_token / bos_token jako dodatkowa heurystyke
    if let Some(eos) = config.get("eos_token").and_then(|v| v.as_str()) {
        if eos.contains("im_end") {
            debug!("Wykryto szablon ChatML (im_end jako eos_token)");
            return ChatTemplate::ChatML;
        }
        if eos.contains("eot_id") {
            debug!("Wykryto szablon Llama3 (eot_id jako eos_token)");
            return ChatTemplate::Llama3;
        }
    }

    warn!(
        "Nie udalo sie wykryc szablonu chatu dla {} — uzywam Plain",
        model_dir.display(),
    );
    ChatTemplate::Plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chatml_format() {
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: "Jestes pomocnym asystentem.".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Czesc!".into(),
            },
        ];

        let result = ChatTemplate::ChatML.format_messages(&messages, true);
        assert!(result.contains("<|im_start|>system\nJestes pomocnym asystentem.<|im_end|>"));
        assert!(result.contains("<|im_start|>user\nCzesc!<|im_end|>"));
        assert!(result.ends_with("<|im_start|>assistant\n"));
    }

    #[test]
    fn test_chatml_without_generation_prompt() {
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: "Test".into(),
        }];

        let result = ChatTemplate::ChatML.format_messages(&messages, false);
        assert!(!result.contains("<|im_start|>assistant"));
        assert!(result.ends_with("<|im_end|>\n"));
    }

    #[test]
    fn test_llama3_format() {
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: "System.".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Pytanie.".into(),
            },
        ];

        let result = ChatTemplate::Llama3.format_messages(&messages, true);
        assert!(result.starts_with("<|begin_of_text|>"));
        assert!(result.contains("<|start_header_id|>system<|end_header_id|>"));
        assert!(result.contains("<|eot_id|>"));
        assert!(result.ends_with("<|start_header_id|>assistant<|end_header_id|>\n\n"));
    }

    #[test]
    fn test_plain_format() {
        let messages = vec![
            ChatMessage {
                role: "system".into(),
                content: "Instrukcja.".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "Hej.".into(),
            },
        ];

        let result = ChatTemplate::Plain.format_messages(&messages, true);
        assert!(result.contains("System: Instrukcja."));
        assert!(result.contains("User: Hej."));
        assert!(result.ends_with("Assistant: "));
    }

    #[test]
    fn test_stop_tokens() {
        assert_eq!(
            ChatTemplate::ChatML.stop_tokens(),
            vec!["<|im_end|>", "<|endoftext|>", "</s>"]
        );
        assert_eq!(ChatTemplate::Llama3.stop_tokens(), vec!["<|eot_id|>"]);
        assert!(ChatTemplate::Plain.stop_tokens().is_empty());
    }

    #[test]
    fn test_template_name() {
        assert_eq!(ChatTemplate::ChatML.name(), "chatml");
        assert_eq!(ChatTemplate::Llama3.name(), "llama3");
        assert_eq!(ChatTemplate::Plain.name(), "plain");
    }
}
