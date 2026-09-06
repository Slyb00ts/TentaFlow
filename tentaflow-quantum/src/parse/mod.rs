// ===== File: parse/mod.rs — OpenQASM 3 front end: subset validation and entry point =====

mod lower;

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};

use oq3_semantics::semantic_error::SemanticErrorKind;
use oq3_semantics::syntax_to_semantics::parse_source_string;
use oq3_syntax::ast::{AstNode, Stmt};
use oq3_syntax::{SourceFile, SyntaxKind, TextRange};

use crate::error::{Error, Result, SourcePos};
use crate::ir::Circuit;

/// Values bound to `input float` parameters for this parse.
pub type InputValues = BTreeMap<String, f64>;

/// The only include this crate resolves; every other one would mean reading a
/// file from disk on behalf of user-supplied source.
const ALLOWED_INCLUDE: &str = "stdgates.inc";

/// Parse OpenQASM 3 into the circuit IR.
///
/// The supported subset is `qubit[n]`/`bit[n]` declarations, `stdgates.inc`
/// gates, user `gate` definitions (inlined), `measure`, `reset`, `barrier`,
/// `if` on classical bits, `for` over a constant range (unrolled) and
/// `input float` parameters. Anything else is reported with the line and column
/// it appears on.
pub fn parse_qasm3(source: &str, inputs: &InputValues) -> Result<Circuit> {
    reject_keywords(source)?;

    // The source is parsed twice on purpose: the subset check needs the syntax
    // tree with its text ranges BEFORE semantic analysis runs, because that pass
    // panics on some of the constructs the check is there to reject.
    let syntax = SourceFile::parse(source);
    if let Some(error) = syntax.errors().first() {
        return Err(Error::Syntax {
            pos: position(source, error.range()),
            message: error.to_string(),
        });
    }
    reject_unsupported(source, &syntax.syntax_node())?;

    // The upstream parser panics on some constructs it declares unsupported
    // (binary logic operators, for one). User-supplied source must not be able
    // to take the process down.
    let parsed = catch_unwind(AssertUnwindSafe(|| {
        parse_source_string(source, None, None::<&[&std::path::Path]>)
    }))
    .map_err(|payload| Error::ParserPanic {
        message: panic_message(payload),
    })?;

    let (program, errors, symbols) = parsed.take_context().as_tuple();
    // Gate arity is re-checked while lowering, and it has to be: the upstream
    // analyser counts the operands of `ctrl @ x a, b` against the arity of `x`
    // and rejects a perfectly valid modified call.
    if let Some(error) = errors.iter().find(|error| {
        !matches!(
            error.kind(),
            SemanticErrorKind::NumGateQubitsError | SemanticErrorKind::NumGateParamsError
        )
    }) {
        return Err(Error::Semantic {
            pos: position(source, error.range()),
            message: error.message(),
        });
    }
    let positions = statement_positions(source, &syntax.tree(), program.stmts().len());
    lower::lower(&program, &symbols, inputs, &positions)
}

/// Reserved words whose syntax the upstream parser cannot even build a tree for:
/// it fails them at the lexical stage with a message that names nothing (`box`
/// comes back as "Expecting semicolon terminating statement"). All five are
/// OpenQASM 3 keywords, so a program inside the supported subset can never carry
/// one as an identifier and a lexical scan is enough to name the construct.
const REJECTED_KEYWORDS: [&str; 5] = ["box", "cal", "defcal", "defcalgrammar", "extern"];

/// Report the first rejected keyword in the source, with its position. Comments,
/// string literals and annotation lines are skipped, so prose that happens to
/// contain one of the words is not a diagnostic.
fn reject_keywords(source: &str) -> Result<()> {
    let bytes = source.as_bytes();
    let mut index = 0usize;
    // An annotation runs to the end of its line and holds free text; `@` in any
    // other column is the gate-modifier marker of `ctrl @ x q[0], q[1];`.
    let mut at_line_start = true;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index = skip_to_line_end(bytes, index);
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index = skip_block_comment(bytes, index);
        } else if byte == b'"' {
            index = skip_string(bytes, index);
        } else if byte == b'@' && at_line_start {
            index = skip_to_line_end(bytes, index);
        } else if is_word_start(byte) {
            let end = word_end(bytes, index);
            let word = &source[index..end];
            if REJECTED_KEYWORDS.contains(&word) {
                return Err(Error::Unsupported {
                    pos: SourcePos::from_offset(source, index),
                    construct: word.to_string(),
                });
            }
            at_line_start = false;
            index = end;
            continue;
        } else {
            if byte == b'\n' {
                at_line_start = true;
            } else if !byte.is_ascii_whitespace() {
                at_line_start = false;
            }
            index += 1;
            continue;
        }
        // A comment or an annotation stops ON its newline, which the branch
        // below turns into a fresh line on the next pass; a string or a block
        // comment ends mid-line, so nothing after it starts one.
        at_line_start = false;
    }
    Ok(())
}

fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte == b'$'
}

fn word_end(bytes: &[u8], start: usize) -> usize {
    let mut end = start;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_' || bytes[end] == b'$')
    {
        end += 1;
    }
    end
}

fn skip_to_line_end(bytes: &[u8], start: usize) -> usize {
    let mut index = start;
    while index < bytes.len() && bytes[index] != b'\n' {
        index += 1;
    }
    index
}

fn skip_block_comment(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 2;
    while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
        index += 1;
    }
    (index + 2).min(bytes.len())
}

fn skip_string(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 1;
    while index < bytes.len() && bytes[index] != b'"' {
        index += if bytes[index] == b'\\' { 2 } else { 1 };
    }
    (index + 1).min(bytes.len())
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "the parser panicked".to_string()
    }
}

/// Source position of every statement in the semantic program, in order.
///
/// The semantic graph carries no text ranges at all, so the only way to point a
/// lowering diagnostic at a line is to line the graph's statement list up with
/// the syntax tree's. The upstream conversion emits one semantic statement per
/// syntax statement except for `include`, the version string and an annotation
/// (which is folded into the statement that follows it). If the two lists still
/// come out different lengths the mapping is not trustworthy and is dropped, so
/// a diagnostic loses its position rather than pointing at the wrong line.
fn statement_positions(source: &str, tree: &SourceFile, expected: usize) -> Vec<SourcePos> {
    let positions: Vec<SourcePos> = tree
        .statements()
        .filter(|stmt| {
            !matches!(
                stmt,
                Stmt::Include(_) | Stmt::VersionString(_) | Stmt::AnnotationStatement(_)
            )
        })
        .map(|stmt| position(source, stmt.syntax().text_range()))
        .collect();
    if positions.len() == expected {
        positions
    } else {
        Vec::new()
    }
}

fn position(source: &str, range: TextRange) -> SourcePos {
    SourcePos::from_offset(source, usize::from(range.start()))
}

/// Walk the syntax tree and reject every construct outside the supported subset
/// before semantic analysis runs, so the diagnostic carries a real position
/// instead of a statement name.
fn reject_unsupported(source: &str, root: &oq3_syntax::SyntaxNode) -> Result<()> {
    for element in root.descendants_with_tokens() {
        if let Some(construct) = unsupported_name(element.kind()) {
            return Err(Error::Unsupported {
                pos: position(source, element.text_range()),
                construct: construct.to_string(),
            });
        }
        if element.kind() == SyntaxKind::INCLUDE {
            let text = element.to_string();
            if !text.contains(ALLOWED_INCLUDE) {
                return Err(Error::Unsupported {
                    pos: position(source, element.text_range()),
                    construct: format!("include other than \"{ALLOWED_INCLUDE}\""),
                });
            }
        }
    }
    Ok(())
}

/// Constructs the parser does build a tree for; `REJECTED_KEYWORDS` covers the
/// ones it cannot.
fn unsupported_name(kind: SyntaxKind) -> Option<&'static str> {
    match kind {
        SyntaxKind::WHILE_STMT => Some("while"),
        SyntaxKind::DEF => Some("def"),
        SyntaxKind::DELAY_STMT => Some("delay"),
        SyntaxKind::DURATION_TY => Some("duration"),
        SyntaxKind::STRETCH_TY => Some("stretch"),
        SyntaxKind::TIMING_LITERAL => Some("timing literal"),
        SyntaxKind::SWITCH_CASE_STMT => Some("switch"),
        SyntaxKind::ALIAS_DECLARATION_STATEMENT => Some("let alias"),
        SyntaxKind::ARRAY_TYPE => Some("array"),
        SyntaxKind::HARDWAREIDENT => Some("hardware qubit"),
        SyntaxKind::OUTPUT_KW => Some("output"),
        SyntaxKind::RETURN_EXPR => Some("return"),
        SyntaxKind::END_STMT => Some("end"),
        SyntaxKind::BREAK_STMT => Some("break"),
        SyntaxKind::CONTINUE_STMT => Some("continue"),
        _ => None,
    }
}
