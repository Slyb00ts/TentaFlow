// ===== File: grammar.rs — request → compiled constrained-decoding program =====
// Bridges the OpenAI request surface (`response_format`, `tool_choice`,
// `grammar`) to the byte-level grammar engine. Compiled automata are cached by
// their canonical constraint string so repeated requests (and the four e2e
// prompts) share one compile. The vocabulary byte table is built once per
// server from the tokenizer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use forge_grammar::builder::{AstRule, Item, Lowerer};
use forge_grammar::schema::SchemaConverter;
use forge_grammar::{Grammar, GrammarProgram, GrammarVocab};
use forge_tokenize::Tokenizer;

use crate::api::{ChatCompletionRequest, ToolForcing};
use crate::error::ApiError;
use crate::toolcall::ToolParserKind;

/// What the request wants the output constrained to.
pub enum ConstraintKind {
    /// Any syntactically valid JSON value.
    JsonObject,
    /// A JSON document conforming to this schema.
    JsonSchema(serde_json::Value),
    /// A GBNF/EBNF grammar (root rule `root`).
    Gbnf(String),
    /// A regex the full output must match.
    Regex(String),
    /// The model must emit a valid tool call (name + argument schema pairs).
    ForcedTools(Vec<(String, serde_json::Value)>),
}

/// Per-server constrained-decoding engine: the shared vocab byte table plus a
/// compiled-automaton cache.
pub struct GrammarEngine {
    vocab: Arc<GrammarVocab>,
    cache: Mutex<HashMap<String, Arc<Grammar>>>,
}

impl GrammarEngine {
    pub fn new(tokenizer: &Tokenizer, eos_ids: &[u32]) -> Self {
        let table = tokenizer.token_byte_table();
        Self {
            vocab: Arc::new(GrammarVocab::new(table, eos_ids)),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Build the grammar program for a chat request, or `None` when it is
    /// unconstrained. The forced-tool wrapper is emitted for the given
    /// tool-call parser syntax.
    pub fn resolve(
        &self,
        req: &ChatCompletionRequest,
        parser: ToolParserKind,
    ) -> Result<Option<GrammarProgram>, ApiError> {
        let Some(kind) = self.constraint_kind(req)? else {
            return Ok(None);
        };
        let grammar = self.compile(&kind, parser)?;
        Ok(Some(GrammarProgram::new(grammar, self.vocab.clone())))
    }

    /// Resolve the request into a single constraint (explicit `grammar` and
    /// `response_format` take precedence over tool forcing).
    fn constraint_kind(
        &self,
        req: &ChatCompletionRequest,
    ) -> Result<Option<ConstraintKind>, ApiError> {
        if let Some(g) = &req.grammar {
            if !g.trim().is_empty() {
                return Ok(Some(ConstraintKind::Gbnf(g.clone())));
            }
        }
        if let Some(rf) = &req.response_format {
            if let Some(kind) = parse_response_format(rf)? {
                return Ok(Some(kind));
            }
        }
        if let Some(forcing) = req.tool_forcing()? {
            let defs = req.tool_definitions();
            let selected: Vec<(String, serde_json::Value)> = match forcing {
                ToolForcing::Any => defs,
                ToolForcing::Named(name) => {
                    let found = defs.into_iter().find(|(n, _)| n == &name).ok_or_else(|| {
                        ApiError::invalid_request(format!(
                            "tool_choice names function {name:?} which is not in `tools`"
                        ))
                    })?;
                    vec![found]
                }
            };
            if selected.is_empty() {
                return Err(ApiError::invalid_request(
                    "tool_choice forcing requires at least one function tool",
                ));
            }
            return Ok(Some(ConstraintKind::ForcedTools(selected)));
        }
        Ok(None)
    }

    fn compile(
        &self,
        kind: &ConstraintKind,
        parser: ToolParserKind,
    ) -> Result<Arc<Grammar>, ApiError> {
        let key = cache_key(kind, parser);
        if let Some(g) = self.cache.lock().expect("grammar cache").get(&key) {
            return Ok(g.clone());
        }
        let grammar = build_grammar(kind, parser)?;
        let arc = Arc::new(grammar);
        self.cache
            .lock()
            .expect("grammar cache")
            .insert(key, arc.clone());
        Ok(arc)
    }
}

fn parse_response_format(rf: &serde_json::Value) -> Result<Option<ConstraintKind>, ApiError> {
    let obj = match rf {
        serde_json::Value::Object(o) => o,
        serde_json::Value::Null => return Ok(None),
        _ => {
            return Err(ApiError::invalid_request(
                "response_format must be an object",
            ))
        }
    };
    let ty = obj
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::invalid_request("response_format.type must be a string"))?;
    match ty {
        "text" => Ok(None),
        "json_object" => Ok(Some(ConstraintKind::JsonObject)),
        "json_schema" => {
            let schema = obj
                .get("json_schema")
                .and_then(|js| js.get("schema"))
                .ok_or_else(|| {
                    ApiError::invalid_request(
                        "response_format json_schema requires json_schema.schema",
                    )
                })?;
            Ok(Some(ConstraintKind::JsonSchema(schema.clone())))
        }
        "regex" => {
            let re = obj
                .get("regex")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ApiError::invalid_request("response_format regex requires `regex`"))?;
            Ok(Some(ConstraintKind::Regex(re.to_string())))
        }
        "grammar" => {
            let g = obj
                .get("grammar")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    ApiError::invalid_request("response_format grammar requires `grammar`")
                })?;
            Ok(Some(ConstraintKind::Gbnf(g.to_string())))
        }
        other => Err(ApiError::invalid_request(format!(
            "unsupported response_format.type {other:?}"
        ))),
    }
}

fn build_grammar(kind: &ConstraintKind, parser: ToolParserKind) -> Result<Grammar, ApiError> {
    let map_err = |e: forge_types::ForgeError| ApiError::invalid_request(e.to_string());
    match kind {
        ConstraintKind::JsonObject => Ok(Grammar::json_value()),
        ConstraintKind::JsonSchema(schema) => Grammar::from_json_schema(schema).map_err(map_err),
        ConstraintKind::Gbnf(src) => Grammar::from_gbnf(src).map_err(map_err),
        ConstraintKind::Regex(re) => Grammar::from_regex(re).map_err(map_err),
        ConstraintKind::ForcedTools(tools) => build_tool_grammar(tools, parser).map_err(map_err),
    }
}

/// Build a grammar whose only sentences are valid tool calls for the model's
/// tool-call syntax, with each tool's arguments constrained to its schema.
fn build_tool_grammar(
    tools: &[(String, serde_json::Value)],
    parser: ToolParserKind,
) -> forge_types::Result<Grammar> {
    if !matches!(parser, ToolParserKind::Hermes) {
        return Err(forge_types::ForgeError::Grammar(
            "forced tool calls are only supported for the Hermes/Qwen tool-call syntax".into(),
        ));
    }
    let mut conv = SchemaConverter::new();
    let mut alternates = Vec::with_capacity(tools.len());
    for (name, params) in tools {
        let args_rule = conv.value_rule(params)?;
        let name_json = serde_json::to_string(name).expect("json string");
        alternates.push(vec![
            Item::literal("<tool_call>\n{\"name\": "),
            Item::literal(&name_json),
            Item::literal(", \"arguments\": "),
            Item::Ref(args_rule),
            Item::literal("}\n</tool_call>"),
        ]);
    }
    conv.push_rule(AstRule {
        name: "root".into(),
        alternates,
    });
    Lowerer::lower(conv.into_rules(), "root")
}

/// A stable string identifying a constraint for the compile cache.
fn cache_key(kind: &ConstraintKind, parser: ToolParserKind) -> String {
    match kind {
        ConstraintKind::JsonObject => "json_object".into(),
        ConstraintKind::JsonSchema(s) => format!("schema:{s}"),
        ConstraintKind::Gbnf(g) => format!("gbnf:{g}"),
        ConstraintKind::Regex(r) => format!("regex:{r}"),
        ConstraintKind::ForcedTools(t) => {
            let mut s = format!("tools:{parser:?}:");
            for (n, p) in t {
                s.push_str(n);
                s.push('=');
                s.push_str(&p.to_string());
                s.push(';');
            }
            s
        }
    }
}
