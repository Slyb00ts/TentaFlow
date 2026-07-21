// ===== File: lib.rs — forge-grammar: byte-level constrained-decoding engine =====
// SPEC §8.1.2 constrained decoding. Three front-ends compile to one shared
// automaton (`grammar::Grammar`): GBNF/EBNF (llama.cpp-compatible), a common
// JSON-Schema subset, and a regex subset. A per-sequence `GrammarMatcher`
// turns the current parse state into a vocab logit mask, so the sampler can
// physically only pick a conforming token; after a token is chosen the state
// is advanced by that token's bytes.

pub mod builder;
pub mod gbnf;
pub mod grammar;
pub mod matcher;
pub mod regex;
pub mod schema;
pub mod vocab;

use std::sync::Arc;

use forge_types::Result;

pub use builder::{AstRule, CharSet, Item};
pub use grammar::Grammar;
pub use matcher::{GrammarMatcher, GrammarProgram};
pub use vocab::GrammarVocab;

impl Grammar {
    /// Compile GBNF/EBNF text. The entry rule must be named `root`.
    pub fn from_gbnf(src: &str) -> Result<Grammar> {
        let ast = gbnf::parse(src)?;
        builder::Lowerer::lower(ast, "root")
    }

    /// Compile a JSON Schema (common subset) into a grammar whose language is
    /// exactly its conforming JSON documents.
    pub fn from_json_schema(schema: &serde_json::Value) -> Result<Grammar> {
        let ast = schema::convert(schema)?;
        builder::Lowerer::lower(ast, "root")
    }

    /// Compile a regex (common subset) matching the full generated span.
    pub fn from_regex(pattern: &str) -> Result<Grammar> {
        let ast = regex::convert(pattern)?;
        builder::Lowerer::lower(ast, "root")
    }

    /// Any syntactically valid JSON value (`response_format: json_object`).
    pub fn json_value() -> Grammar {
        let schema = serde_json::json!({});
        Grammar::from_json_schema(&schema).expect("empty schema compiles")
    }
}

/// Bind a compiled grammar to a vocabulary, ready to drive decoding.
pub fn program(grammar: Grammar, vocab: Arc<GrammarVocab>) -> GrammarProgram {
    GrammarProgram::new(Arc::new(grammar), vocab)
}

#[cfg(test)]
mod tests;
