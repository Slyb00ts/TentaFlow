// ===== File: chat.rs — HF-compatible chat templating (minijinja, pycompat, sandboxed) =====
//
// Renders HF `chat_template` Jinja sources with the Python-isms real-world
// templates rely on (`.strip()`, `.split()`, `namespace()`, `messages[::-1]`,
// `raise_exception`, `tojson`, `strftime_now`). The environment is sandboxed:
// no template loader (no filesystem), no env access, and a fuel limit bounds
// runaway loops from untrusted template sources.

use forge_types::{ForgeError, Result};
use minijinja::value::Value;
use minijinja::{Environment, Error as JinjaError, ErrorKind, State, UndefinedBehavior};
use serde::{Deserialize, Serialize};

/// One chat turn in the HF `apply_chat_template` message shape.
/// `content` is either a string or a multipart array of `{type, text, ...}` parts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Extra per-message fields some templates read (e.g. `reasoning_content`).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ChatMessage {
    pub fn text(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(serde_json::Value::String(content.into())),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            extra: serde_json::Map::new(),
        }
    }

    pub fn parts(role: impl Into<String>, parts: Vec<serde_json::Value>) -> Self {
        Self {
            role: role.into(),
            content: Some(serde_json::Value::Array(parts)),
            name: None,
            tool_calls: None,
            tool_call_id: None,
            extra: serde_json::Map::new(),
        }
    }

    /// Plain text of the message: the string content or the concatenated
    /// `text` parts of a multipart content array.
    pub fn text_content(&self) -> Option<String> {
        match &self.content {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Array(parts)) => {
                let joined: String = parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect();
                Some(joined)
            }
            _ => None,
        }
    }
}

pub struct ChatTemplateEngine {
    fuel: u64,
}

impl Default for ChatTemplateEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatTemplateEngine {
    pub fn new() -> Self {
        // Generous for any realistic conversation; a hard bound for malicious
        // or buggy templates (fuel is decremented per VM instruction).
        Self { fuel: 5_000_000 }
    }

    pub fn with_fuel(fuel: u64) -> Self {
        Self { fuel }
    }

    pub fn render(
        &self,
        template_src: &str,
        messages: &[ChatMessage],
        tools: Option<&serde_json::Value>,
        add_generation_prompt: bool,
        continue_final_message: bool,
        extra_vars: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<String> {
        if add_generation_prompt && continue_final_message {
            return Err(ForgeError::Tokenizer(
                "add_generation_prompt and continue_final_message are mutually exclusive".into(),
            ));
        }

        let mut env = Environment::new();
        env.set_fuel(Some(self.fuel));
        // HF renders with ChainableUndefined so templates can probe optional
        // fields (`message.tool_calls`) without exploding.
        env.set_undefined_behavior(UndefinedBehavior::Chainable);
        env.set_unknown_method_callback(unknown_method_callback);
        env.add_function("raise_exception", raise_exception);
        env.add_function("strftime_now", strftime_now);
        env.add_template("chat", template_src)
            .map_err(|e| ForgeError::Tokenizer(format!("chat template parse error: {e:#}")))?;

        let mut ctx = std::collections::BTreeMap::new();
        for (k, v) in extra_vars {
            ctx.insert(k.clone(), Value::from_serialize(v));
        }
        ctx.insert("messages".into(), Value::from_serialize(messages));
        ctx.insert(
            "tools".into(),
            match tools {
                Some(t) => Value::from_serialize(t),
                None => Value::from(()),
            },
        );
        ctx.insert(
            "add_generation_prompt".into(),
            Value::from(add_generation_prompt),
        );

        let template = env.get_template("chat").expect("template just added");
        let mut rendered = template
            .render(Value::from_serialize(&ctx))
            .map_err(|e| ForgeError::Tokenizer(format!("chat template render error: {e:#}")))?;

        if continue_final_message {
            rendered = truncate_after_final_message(rendered, messages)?;
        }
        Ok(rendered)
    }
}

/// HF `continue_final_message` semantics: render normally, then cut the output
/// right after the final message's content so the model continues that turn
/// instead of starting a new one.
fn truncate_after_final_message(rendered: String, messages: &[ChatMessage]) -> Result<String> {
    let last = messages.last().ok_or_else(|| {
        ForgeError::Tokenizer("continue_final_message requires at least one message".into())
    })?;
    let final_text = last.text_content().ok_or_else(|| {
        ForgeError::Tokenizer(
            "continue_final_message requires the final message to have text content".into(),
        )
    })?;
    // Templates may trim message content, so match on the trimmed text.
    let needle = final_text.trim();
    if needle.is_empty() {
        return Err(ForgeError::Tokenizer(
            "continue_final_message requires non-empty final message content".into(),
        ));
    }
    let idx = rendered.rfind(needle).ok_or_else(|| {
        ForgeError::Tokenizer(
            "continue_final_message: final message content not found in rendered template".into(),
        )
    })?;
    let mut out = rendered;
    out.truncate(idx + needle.len());
    Ok(out)
}

fn raise_exception(msg: String) -> std::result::Result<Value, JinjaError> {
    Err(JinjaError::new(ErrorKind::InvalidOperation, msg))
}

fn strftime_now(fmt: String) -> std::result::Result<Value, JinjaError> {
    use chrono::format::{Item, StrftimeItems};
    let items: Vec<Item<'_>> = StrftimeItems::new(&fmt).collect();
    if items.iter().any(|i| matches!(i, Item::Error)) {
        return Err(JinjaError::new(
            ErrorKind::InvalidOperation,
            format!("strftime_now: invalid format string {fmt:?}"),
        ));
    }
    let now = chrono::Local::now();
    Ok(Value::from(now.format_with_items(items.iter()).to_string()))
}

/// Python string methods used by real HF chat templates. Implemented here
/// (with the char-set arguments minijinja-contrib's pycompat lacks for
/// strip/lstrip/rstrip) and delegating everything else — `.items()`,
/// `.get()`, `.keys()`, `.upper()`, ... — to pycompat.
fn unknown_method_callback(
    state: &State,
    value: &Value,
    method: &str,
    args: &[Value],
) -> std::result::Result<Value, JinjaError> {
    if let Some(s) = value.as_str() {
        match method {
            "strip" | "lstrip" | "rstrip" => {
                let chars: Option<String> = optional_str_arg(args, method)?;
                let matches_set = |c: char| match &chars {
                    Some(set) => set.contains(c),
                    None => c.is_whitespace(),
                };
                let out = match method {
                    "strip" => s.trim_matches(matches_set),
                    "lstrip" => s.trim_start_matches(matches_set),
                    _ => s.trim_end_matches(matches_set),
                };
                return Ok(Value::from(out));
            }
            "split" => {
                let (sep, maxsplit) = split_args(args)?;
                let parts: Vec<String> = match sep {
                    None => s.split_whitespace().map(str::to_string).collect(),
                    Some(sep) if sep.is_empty() => {
                        return Err(JinjaError::new(
                            ErrorKind::InvalidOperation,
                            "split: empty separator",
                        ))
                    }
                    Some(sep) => match maxsplit {
                        Some(n) => s.splitn(n + 1, sep.as_str()).map(str::to_string).collect(),
                        None => s.split(sep.as_str()).map(str::to_string).collect(),
                    },
                };
                return Ok(Value::from(parts));
            }
            "title" => {
                let mut out = String::with_capacity(s.len());
                let mut prev_alpha = false;
                for c in s.chars() {
                    if c.is_alphabetic() {
                        if prev_alpha {
                            out.extend(c.to_lowercase());
                        } else {
                            out.extend(c.to_uppercase());
                        }
                        prev_alpha = true;
                    } else {
                        out.push(c);
                        prev_alpha = false;
                    }
                }
                return Ok(Value::from(out));
            }
            "startswith" | "endswith" => {
                let check = |needle: &str| {
                    if method == "startswith" {
                        s.starts_with(needle)
                    } else {
                        s.ends_with(needle)
                    }
                };
                let arg = args.first().ok_or_else(|| {
                    JinjaError::new(
                        ErrorKind::MissingArgument,
                        format!("{method} requires an argument"),
                    )
                })?;
                if let Some(needle) = arg.as_str() {
                    return Ok(Value::from(check(needle)));
                }
                // Python accepts a tuple of prefixes/suffixes.
                if let Ok(iter) = arg.try_iter() {
                    let hit = iter.filter_map(|v| v.as_str().map(&check)).any(|b| b);
                    return Ok(Value::from(hit));
                }
                return Err(JinjaError::new(
                    ErrorKind::InvalidOperation,
                    format!("{method}: argument must be a string or sequence of strings"),
                ));
            }
            "replace" => {
                let old = required_str_arg(args, 0, "replace")?;
                let new = required_str_arg(args, 1, "replace")?;
                let count = args
                    .get(2)
                    .map(|v| {
                        i64::try_from(v.clone()).map_err(|_| {
                            JinjaError::new(
                                ErrorKind::InvalidOperation,
                                "replace: count must be an integer",
                            )
                        })
                    })
                    .transpose()?;
                let out = match count {
                    Some(n) if n >= 0 => s.replacen(&old, &new, n as usize),
                    _ => s.replace(&old, &new),
                };
                return Ok(Value::from(out));
            }
            _ => {}
        }
    }
    minijinja_contrib::pycompat::unknown_method_callback(state, value, method, args)
}

fn optional_str_arg(
    args: &[Value],
    method: &str,
) -> std::result::Result<Option<String>, JinjaError> {
    match args.first() {
        None => Ok(None),
        Some(v) if v.is_none() => Ok(None),
        Some(v) => v.as_str().map(|s| Some(s.to_string())).ok_or_else(|| {
            JinjaError::new(
                ErrorKind::InvalidOperation,
                format!("{method}: argument must be a string"),
            )
        }),
    }
}

fn required_str_arg(
    args: &[Value],
    idx: usize,
    method: &str,
) -> std::result::Result<String, JinjaError> {
    args.get(idx)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| {
            JinjaError::new(
                ErrorKind::InvalidOperation,
                format!("{method}: argument {idx} must be a string"),
            )
        })
}

fn split_args(args: &[Value]) -> std::result::Result<(Option<String>, Option<usize>), JinjaError> {
    let sep = match args.first() {
        None => None,
        Some(v) if v.is_none() => None,
        Some(v) => Some(v.as_str().map(str::to_string).ok_or_else(|| {
            JinjaError::new(
                ErrorKind::InvalidOperation,
                "split: separator must be a string",
            )
        })?),
    };
    let maxsplit = match args.get(1) {
        None => None,
        Some(v) => {
            let n = i64::try_from(v.clone()).map_err(|_| {
                JinjaError::new(
                    ErrorKind::InvalidOperation,
                    "split: maxsplit must be an integer",
                )
            })?;
            if n < 0 {
                None
            } else {
                Some(n as usize)
            }
        }
    };
    Ok((sep, maxsplit))
}

/// Resolve which chat template source to render, in HF-compatible priority
/// order: explicit request override → tokenizer_config.json `chat_template` →
/// GGUF `tokenizer.chat_template` metadata → built-in registry by family.
pub fn resolve_chat_template<'a>(
    override_src: Option<&'a str>,
    tokenizer_config_template: Option<&'a str>,
    gguf_template: Option<&'a str>,
    family: Option<&str>,
) -> Result<&'a str> {
    if let Some(src) = override_src {
        return Ok(src);
    }
    if let Some(src) = tokenizer_config_template {
        return Ok(src);
    }
    if let Some(src) = gguf_template {
        return Ok(src);
    }
    if let Some(family) = family {
        if let Some(src) = builtin_chat_template(family) {
            return Ok(src);
        }
        return Err(ForgeError::Tokenizer(format!(
            "no chat template available: unknown builtin family {family:?}"
        )));
    }
    Err(ForgeError::Tokenizer(
        "no chat template available: no override, tokenizer_config, GGUF template or family".into(),
    ))
}

/// Built-in chat template registry (upstream-equivalent template sources).
pub fn builtin_chat_template(family: &str) -> Option<&'static str> {
    match family {
        "chatml" => Some(CHATML_TEMPLATE),
        "llama3" => Some(LLAMA3_TEMPLATE),
        "mistral" => Some(MISTRAL_TEMPLATE),
        "gemma" => Some(GEMMA_TEMPLATE),
        "qwen" => Some(QWEN_TEMPLATE),
        _ => None,
    }
}

const CHATML_TEMPLATE: &str = r#"{% for message in messages %}{{ '<|im_start|>' + message['role'] + '
' + message['content'] + '<|im_end|>' + '
' }}{% endfor %}{% if add_generation_prompt %}{{ '<|im_start|>assistant
' }}{% endif %}"#;

// Upstream meta-llama/Meta-Llama-3-8B-Instruct template.
const LLAMA3_TEMPLATE: &str = r#"{% set loop_messages = messages %}{% for message in loop_messages %}{% set content = '<|start_header_id|>' + message['role'] + '<|end_header_id|>

'+ message['content'] | trim + '<|eot_id|>' %}{% if loop.index0 == 0 %}{% set content = bos_token + content %}{% endif %}{{ content }}{% endfor %}{% if add_generation_prompt %}{{ '<|start_header_id|>assistant<|end_header_id|>

' }}{% endif %}"#;

// Upstream mistralai/Mistral-7B-Instruct-v0.1 template.
const MISTRAL_TEMPLATE: &str = r#"{{ bos_token }}{% for message in messages %}{% if (message['role'] == 'user') != (loop.index0 % 2 == 0) %}{{ raise_exception('Conversation roles must alternate user/assistant/user/assistant/...') }}{% endif %}{% if message['role'] == 'user' %}{{ '[INST] ' + message['content'] + ' [/INST]' }}{% elif message['role'] == 'assistant' %}{{ message['content'] + eos_token }}{% else %}{{ raise_exception('Only user and assistant roles are supported!') }}{% endif %}{% endfor %}"#;

// Upstream google/gemma-2-it template.
const GEMMA_TEMPLATE: &str = r#"{{ bos_token }}{% if messages[0]['role'] == 'system' %}{{ raise_exception('System role not supported') }}{% endif %}{% for message in messages %}{% if (message['role'] == 'user') != (loop.index0 % 2 == 0) %}{{ raise_exception('Conversation roles must alternate user/assistant/user/assistant/...') }}{% endif %}{% if (message['role'] == 'assistant') %}{% set role = 'model' %}{% else %}{% set role = message['role'] %}{% endif %}{{ '<start_of_turn>' + role + '
' + message['content'] | trim + '<end_of_turn>
' }}{% endfor %}{% if add_generation_prompt %}{{'<start_of_turn>model
'}}{% endif %}"#;

// Upstream Qwen2.5-Instruct template (ChatML with Hermes-style tool calling).
const QWEN_TEMPLATE: &str = r#"{%- if tools %}
    {{- '<|im_start|>system\n' }}
    {%- if messages[0]['role'] == 'system' %}
        {{- messages[0]['content'] }}
    {%- else %}
        {{- 'You are Qwen, created by Alibaba Cloud. You are a helpful assistant.' }}
    {%- endif %}
    {{- "\n\n# Tools\n\nYou may call one or more functions to assist with the user query.\n\nYou are provided with function signatures within <tools></tools> XML tags:\n<tools>" }}
    {%- for tool in tools %}
        {{- "\n" }}
        {{- tool | tojson }}
    {%- endfor %}
    {{- "\n</tools>\n\nFor each function call, return a json object with function name and arguments within <tool_call></tool_call> XML tags:\n<tool_call>\n{\"name\": <function-name>, \"arguments\": <args-json-object>}\n</tool_call><|im_end|>\n" }}
{%- else %}
    {%- if messages[0]['role'] == 'system' %}
        {{- '<|im_start|>system\n' + messages[0]['content'] + '<|im_end|>\n' }}
    {%- else %}
        {{- '<|im_start|>system\nYou are Qwen, created by Alibaba Cloud. You are a helpful assistant.<|im_end|>\n' }}
    {%- endif %}
{%- endif %}
{%- for message in messages %}
    {%- if (message.role == "user") or (message.role == "system" and not loop.first) or (message.role == "assistant" and not message.tool_calls) %}
        {{- '<|im_start|>' + message.role + '\n' + message.content + '<|im_end|>' + '\n' }}
    {%- elif message.role == "assistant" %}
        {{- '<|im_start|>' + message.role }}
        {%- if message.content %}
            {{- '\n' + message.content }}
        {%- endif %}
        {%- for tool_call in message.tool_calls %}
            {%- if tool_call.function is defined %}
                {%- set tool_call = tool_call.function %}
            {%- endif %}
            {{- '\n<tool_call>\n{"name": "' }}
            {{- tool_call.name }}
            {{- '", "arguments": ' }}
            {{- tool_call.arguments | tojson }}
            {{- '}\n</tool_call>' }}
        {%- endfor %}
        {{- '<|im_end|>\n' }}
    {%- elif message.role == "tool" %}
        {%- if (loop.index0 == 0) or (messages[loop.index0 - 1].role != "tool") %}
            {{- '<|im_start|>user' }}
        {%- endif %}
        {{- '\n<tool_response>\n' }}
        {{- message.content }}
        {{- '\n</tool_response>' }}
        {%- if loop.last or (messages[loop.index0 + 1].role != "tool") %}
            {{- '<|im_end|>\n' }}
        {%- endif %}
    {%- endif %}
{%- endfor %}
{%- if add_generation_prompt %}
    {{- '<|im_start|>assistant\n' }}
{%- endif %}"#;
