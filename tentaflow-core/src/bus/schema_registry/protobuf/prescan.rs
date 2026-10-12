// =============================================================================
// File: bus/schema_registry/protobuf/prescan.rs — token-level pre-scan of a .proto text
// =============================================================================
// Runs on the schema text BEFORE `protobuf-parse` sees it and refuses what
// would crash, overflow or stall the parser (see the header of the parent
// module for what that is).
//
// The scan is built on the parser's OWN lexer (`protobuf-support`, the one
// `protobuf-parse` runs): comments (`//`, and `/* */` which does not nest),
// adjacent string literals (`"a" /* x */ "b"` is one token), numbers and
// identifiers are cut exactly where the parser cuts them. A byte-level scan
// of its own disagrees with the lexer on the first comment placed between two
// tokens (`import /**/ "schema.proto";`), which is how a statement keyword
// slipped past an earlier version. Everything structural is decided on the
// token stream:
//   1. `lex` cuts the text into tokens, checks the lexical caps and that
//      everything between two tokens is blanks and comments;
//   2. `Structure` walks the tokens the way the parser's statement loops do
//      (file, message, oneof and enum bodies) with an explicit stack, records
//      the type names of each scope and the entry type of every `map` field,
//      and refuses what it cannot classify.
// A statement the walk does not recognise is refused ("fail closed"): it is
// not worth a parser run, because the parser would refuse it too, and a
// disagreement between the two would otherwise leave the rest of the text
// unchecked. The walk is deliberately lax INSIDE a statement it understands
// (the value of an option, the rest of a field after its name): that is
// exact for every text the parser accepts and the parser refuses the others.
// =============================================================================

use std::collections::{BTreeMap, HashSet};

use ::protobuf_support::lexer::lexer_impl::Lexer;
use ::protobuf_support::lexer::loc::Loc;
use ::protobuf_support::lexer::parser_language::ParserLanguage;
use ::protobuf_support::lexer::token::Token;

use super::{
    invalid, SchemaError, DESIGNATION_PREFIX, MAX_ASSIGNMENTS, MAX_BRACKET_DEPTH, MAX_ENUMS,
    MAX_ENUM_VALUES, MAX_ENUM_VALUES_PER_ENUM, MAX_FIELDS, MAX_FIELDS_PER_MESSAGE, MAX_MESSAGES,
    MAX_MESSAGE_NESTING, MAX_NAME_LEN, MAX_PACKAGE_LEN, MAX_PACKAGE_PARTS, MAX_QUALIFIED_NAME_LEN,
    MAX_STRING_LITERAL, MAX_TYPE_REF_LEN, MAX_TYPE_REF_PARTS,
};

const BAD_CHARACTER: &str = "the schema contains a character a .proto file cannot";
const DOTTED_OPTION: &str = "option names with a dot are not supported";
const COMMENT_OPEN: &str = "a comment is never closed";
const STRING_CHARACTER: &str =
    "the schema contains a character a .proto file cannot hold inside a string";
const STRING_OPEN: &str = "a string is never closed on its line";

pub(super) struct Prescan {
    pub(super) designated: Option<String>,
}

struct Tok {
    token: Token,
    line: u32,
}

struct Lexed {
    toks: Vec<Tok>,
    designated: Option<String>,
}

pub(super) fn prescan(text: &str) -> Result<Prescan, SchemaError> {
    let lexed = lex(text)?;
    Structure::new(&lexed.toks).run()?;
    Ok(Prescan {
        designated: lexed.designated,
    })
}

// =============================================================================
// Lexical pass
// =============================================================================

/// Comments seen between tokens, keyed by offset: a gap can be read twice
/// (once to find where a string literal ends, once as the gap of its token),
/// and a designation must count once.
#[derive(Default)]
struct Gaps {
    designations: BTreeMap<usize, String>,
}

impl Gaps {
    /// Skips blanks and comments from `at` and returns the offset of the
    /// first byte that is neither.
    fn skip(&mut self, text: &str, mut at: usize) -> Result<usize, SchemaError> {
        let b = text.as_bytes();
        loop {
            match b.get(at) {
                Some(b' ' | b'\t' | b'\r' | b'\n') => at += 1,
                Some(b'/') if b.get(at + 1) == Some(&b'/') => {
                    let end = b[at..]
                        .iter()
                        .position(|&x| x == b'\n')
                        .map_or(b.len(), |p| at + p);
                    self.designation(&text[at + 2..end], at)?;
                    at = end;
                }
                Some(b'/') if b.get(at + 1) == Some(&b'*') => {
                    let Some(close) = text[at + 2..].find("*/") else {
                        return Err(invalid(COMMENT_OPEN));
                    };
                    at += 2 + close + 2;
                }
                _ => return Ok(at),
            }
        }
    }

    fn designation(&mut self, comment: &str, at: usize) -> Result<(), SchemaError> {
        let Some(rest) = comment.trim().strip_prefix(DESIGNATION_PREFIX) else {
            return Ok(());
        };
        if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
            return Ok(());
        }
        let name = rest.trim();
        if name.is_empty()
            || !name.bytes().all(is_ident_byte)
            || name.as_bytes()[0].is_ascii_digit()
            || name.len() > MAX_NAME_LEN
        {
            return Err(invalid(
                "the comment '// tentabus:message' must be followed by the name of a message",
            ));
        }
        self.designations.insert(at, name.to_string());
        Ok(())
    }
}

/// C0, DEL and C1 controls, plus zero-width, bidi and other format characters.
fn is_unsafe_string_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{61C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Where the lexer's (line, column) position lies in the text, advanced
/// monotonically so the whole pass stays linear.
struct Cursor {
    off: usize,
    line: u32,
    col: u32,
}

impl Cursor {
    fn advance_to(&mut self, text: &str, loc: Loc) -> Option<usize> {
        while self.line != loc.line || self.col != loc.col {
            let c = text[self.off..].chars().next()?;
            self.off += c.len_utf8();
            if c == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
        }
        Some(self.off)
    }
}

/// End of one quoted piece of a string literal, from its opening quote.
fn quoted_end(text: &str, start: usize) -> Result<usize, SchemaError> {
    let b = text.as_bytes();
    let quote = b[start];
    let mut at = start + 1;
    loop {
        match b.get(at) {
            None => return Err(invalid(STRING_OPEN)),
            Some(b'\\') => {
                at += 1 + text[at + 1..].chars().next().map_or(0, char::len_utf8);
            }
            Some(&c) if c == quote => return Ok(at + 1),
            Some(_) => at += 1,
        }
    }
}

/// End of a string literal: the lexer joins adjacent literals, with blanks
/// and comments allowed between the pieces.
fn string_end(text: &str, start: usize, gaps: &mut Gaps) -> Result<usize, SchemaError> {
    let mut end = quoted_end(text, start)?;
    loop {
        let after = gaps.skip(text, end)?;
        match text.as_bytes().get(after) {
            Some(b'"' | b'\'') => end = quoted_end(text, after)?,
            _ => return Ok(end),
        }
    }
}

/// An upper bound on where a number ends (the next token start clamps it):
/// the lexer's number alphabet, with a sign only right after an exponent.
fn number_end(text: &str, start: usize) -> usize {
    let b = text.as_bytes();
    let mut at = start;
    while at < b.len() {
        let c = b[at];
        let sign_after_exponent =
            (c == b'+' || c == b'-') && at > start && matches!(b[at - 1], b'e' | b'E');
        if c.is_ascii_alphanumeric() || c == b'.' || c == b'_' || sign_after_exponent {
            at += 1;
        } else {
            break;
        }
    }
    at
}

fn token_end(
    text: &str,
    start: usize,
    token: &Token,
    gaps: &mut Gaps,
) -> Result<usize, SchemaError> {
    Ok(match token {
        Token::Ident(name) => start + name.len(),
        Token::Symbol(_) => start + 1,
        Token::StrLit(_) => string_end(text, start, gaps)?,
        Token::IntLit(_) | Token::FloatLit(_) => number_end(text, start),
        Token::JsonNumber(_) => return Err(invalid(BAD_CHARACTER)),
    })
}

/// What the lexer's refusal was about. The lexer reads a token and then the
/// blanks after it, so the failure is either in the token that follows the
/// last good one or in the comment behind it: the text is walked from there
/// with the same rules.
fn lex_failure(text: &str, from: usize) -> SchemaError {
    let mut gaps = Gaps::default();
    let walk = |gaps: &mut Gaps| -> Result<Option<SchemaError>, SchemaError> {
        let at = gaps.skip(text, from)?;
        let Some(&first) = text.as_bytes().get(at) else {
            return Ok(None);
        };
        let end = match first {
            b'"' | b'\'' => string_end(text, at, gaps)?,
            c if c.is_ascii_digit() || c == b'.' => {
                gaps.skip(text, number_end(text, at))?;
                return Ok(Some(invalid(
                    "not a valid Protobuf schema: a number is malformed or too large",
                )));
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                at + text.as_bytes()[at..]
                    .iter()
                    .take_while(|b| is_ident_byte(**b))
                    .count()
            }
            c if c.is_ascii_punctuation() => at + 1,
            _ => return Ok(None),
        };
        gaps.skip(text, end)?;
        Ok(None)
    };
    match walk(&mut gaps) {
        Ok(found) => found.unwrap_or_else(|| invalid(BAD_CHARACTER)),
        Err(error) => error,
    }
}

fn lex(text: &str) -> Result<Lexed, SchemaError> {
    let mut lexer = Lexer::new(text, ParserLanguage::Proto);
    let mut cursor = Cursor {
        off: 0,
        line: 1,
        col: 1,
    };
    let mut gaps = Gaps::default();
    let mut toks = Vec::new();
    let mut assignments = 0usize;
    // Where the blanks and comments before the next token begin.
    let mut gap_from = 0usize;
    loop {
        let next = match lexer.next_token() {
            Ok(next) => next,
            Err(_) => return Err(lex_failure(text, gap_from)),
        };
        let next_start = match &next {
            Some(t) => cursor
                .advance_to(text, t.loc)
                .ok_or_else(|| invalid(BAD_CHARACTER))?,
            None => text.len(),
        };
        // Only blanks and comments may lie between two tokens: anything else
        // (a non-ASCII blank, a stray control byte) is a place the lexer and
        // this scan could read differently.
        if gaps.skip(text, gap_from.min(next_start))? != next_start {
            return Err(invalid(BAD_CHARACTER));
        }
        let Some(t) = next else { break };
        match &t.token {
            Token::Ident(name) => {
                if !name.is_ascii() {
                    return Err(invalid(BAD_CHARACTER));
                }
                if name.len() > MAX_NAME_LEN {
                    return Err(invalid(format!(
                        "a name is longer than {MAX_NAME_LEN} characters"
                    )));
                }
            }
            Token::StrLit(literal) => {
                if literal.escaped.len() > MAX_STRING_LITERAL {
                    return Err(invalid(format!(
                        "a string is longer than {MAX_STRING_LITERAL} bytes"
                    )));
                }
                // A literal is echoed by some parser refusals: a character
                // that moves a cursor or reorders text must not get that far.
                if literal.escaped.chars().any(is_unsafe_string_char) {
                    return Err(invalid(STRING_CHARACTER));
                }
            }
            Token::Symbol('=') => {
                assignments += 1;
                if assignments > MAX_ASSIGNMENTS {
                    return Err(invalid(format!(
                        "the schema has more than {MAX_ASSIGNMENTS} assignments (field numbers, enum values and options)"
                    )));
                }
            }
            _ => {}
        }
        gap_from = token_end(text, next_start, &t.token, &mut gaps)?;
        toks.push(Tok {
            token: t.token,
            line: t.loc.line,
        });
    }
    if gaps.designations.len() > 1 {
        return Err(invalid(
            "the comment '// tentabus:message' appears more than once",
        ));
    }
    Ok(Lexed {
        toks,
        designated: gaps.designations.into_values().next(),
    })
}

// =============================================================================
// Structural pass
// =============================================================================

#[derive(Clone, Copy, PartialEq)]
enum Frame {
    Message,
    Oneof,
    Enum,
}

struct Structure<'a> {
    toks: &'a [Tok],
    at: usize,
    frames: Vec<Frame>,
    /// Names of the open messages, outermost first.
    scope: Vec<&'a str>,
    /// Fully qualified names of the types declared so far (messages, enums and
    /// the entry type of every map field). A file has ONE package, so the
    /// file-level names share a scope whatever the `package` statement says.
    declared: HashSet<String>,
    messages: usize,
    enums: usize,
    /// Field statements of every open message, parallel to `scope`; a oneof's
    /// members belong to the message around it.
    field_counts: Vec<usize>,
    fields: usize,
    /// Name and value count of the open enum (an enum holds no other frame).
    enum_name: &'a str,
    enum_values: usize,
    total_enum_values: usize,
    /// Bytes of the package name (0 without one).
    package_len: usize,
}

impl<'a> Structure<'a> {
    fn new(toks: &'a [Tok]) -> Self {
        Structure {
            toks,
            at: 0,
            frames: Vec::new(),
            scope: Vec::new(),
            declared: HashSet::new(),
            messages: 0,
            enums: 0,
            field_counts: Vec::new(),
            fields: 0,
            enum_name: "",
            enum_values: 0,
            total_enum_values: 0,
            package_len: 0,
        }
    }

    fn run(mut self) -> Result<(), SchemaError> {
        if self.ident_is("syntax") {
            self.at += 1;
            self.symbol('=')?;
            self.string()?;
            self.symbol(';')?;
        }
        while self.at < self.toks.len() {
            match self.frames.last().copied() {
                None => self.file_statement()?,
                Some(Frame::Message) => self.message_statement()?,
                Some(Frame::Oneof) => self.oneof_statement()?,
                Some(Frame::Enum) => self.enum_statement()?,
            }
            if self.messages > MAX_MESSAGES {
                return Err(invalid(format!(
                    "the schema has more than {MAX_MESSAGES} messages (a map field counts as one)"
                )));
            }
            if self.enums > MAX_ENUMS {
                return Err(invalid(format!(
                    "the schema has more than {MAX_ENUMS} enums"
                )));
            }
        }
        if !self.frames.is_empty() {
            return Err(invalid("a bracket is never closed"));
        }
        Ok(())
    }

    // ---- token access -------------------------------------------------------

    fn peek(&self) -> Option<&'a Token> {
        self.toks.get(self.at).map(|t| &t.token)
    }

    fn ident_is(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Token::Ident(name)) if name == word)
    }

    fn symbol_is(&self, symbol: char) -> bool {
        matches!(self.peek(), Some(Token::Symbol(c)) if *c == symbol)
    }

    fn eat_symbol(&mut self, symbol: char) -> bool {
        let is = self.symbol_is(symbol);
        if is {
            self.at += 1;
        }
        is
    }

    fn symbol(&mut self, symbol: char) -> Result<(), SchemaError> {
        if self.eat_symbol(symbol) {
            Ok(())
        } else {
            Err(self.unexpected())
        }
    }

    fn ident(&mut self) -> Result<&'a str, SchemaError> {
        match self.peek() {
            Some(Token::Ident(name)) => {
                self.at += 1;
                Ok(name)
            }
            _ => Err(self.unexpected()),
        }
    }

    fn string(&mut self) -> Result<(), SchemaError> {
        match self.peek() {
            Some(Token::StrLit(_)) => {
                self.at += 1;
                Ok(())
            }
            _ => Err(self.unexpected()),
        }
    }

    /// The refusal for a statement the walk cannot classify. A `(` is always
    /// the start of a custom option (or a service method), which are refused
    /// by name.
    fn unexpected(&self) -> SchemaError {
        if self.symbol_is('(') {
            return invalid("custom options are not supported");
        }
        match self.toks.get(self.at).or(self.toks.last()) {
            Some(tok) => invalid(format!(
                "not a valid Protobuf schema (line {}): the text does not follow the .proto grammar here",
                tok.line
            )),
            None => invalid("not a valid Protobuf schema: the text ends too early"),
        }
    }

    fn check_depth(&self, local: usize) -> Result<(), SchemaError> {
        if self.frames.len() + local > MAX_BRACKET_DEPTH {
            return Err(invalid(format!(
                "the schema is nested deeper than {MAX_BRACKET_DEPTH} levels"
            )));
        }
        Ok(())
    }

    // ---- declarations -------------------------------------------------------

    fn declare(&mut self, name: &str) -> Result<(), SchemaError> {
        let mut full = self.scope.join(".");
        if !full.is_empty() {
            full.push('.');
        }
        full.push_str(name);
        if self.package_len + full.len() > MAX_QUALIFIED_NAME_LEN {
            return Err(invalid(format!(
                "a name is longer than {MAX_QUALIFIED_NAME_LEN} characters once its package and enclosing messages are counted"
            )));
        }
        if self.declared.insert(full) {
            Ok(())
        } else {
            Err(invalid(format!("type '{name}' is defined more than once")))
        }
    }

    fn open_message(&mut self) -> Result<(), SchemaError> {
        self.at += 1;
        let name = self.ident()?;
        self.declare(name)?;
        self.messages += 1;
        self.symbol('{')?;
        self.scope.push(name);
        self.field_counts.push(0);
        self.frames.push(Frame::Message);
        self.check_depth(0)?;
        if self.scope.len() > MAX_MESSAGE_NESTING {
            return Err(invalid(format!(
                "messages are nested deeper than {MAX_MESSAGE_NESTING} levels"
            )));
        }
        Ok(())
    }

    fn open_enum(&mut self) -> Result<(), SchemaError> {
        self.at += 1;
        let name = self.ident()?;
        self.declare(name)?;
        self.enums += 1;
        self.enum_name = name;
        self.enum_values = 0;
        self.symbol('{')?;
        self.frames.push(Frame::Enum);
        self.check_depth(0)
    }

    fn close(&mut self) {
        self.at += 1;
        if self.frames.pop() == Some(Frame::Message) {
            self.scope.pop();
            self.field_counts.pop();
        }
    }

    // ---- statements ---------------------------------------------------------

    fn file_statement(&mut self) -> Result<(), SchemaError> {
        match self.peek() {
            Some(Token::Symbol(';')) => self.at += 1,
            Some(Token::Symbol('}')) => {
                return Err(invalid("a closing bracket has no opening one"))
            }
            Some(Token::Ident(word)) => match word.as_str() {
                "import" => {
                    return Err(invalid(
                        "import is not supported: a schema must be one self-contained file",
                    ))
                }
                "service" => return Err(invalid("service is not supported")),
                "extend" => return Err(invalid("extend is not supported")),
                "package" => {
                    self.at += 1;
                    self.package_len = self.dotted_name(MAX_PACKAGE_PARTS, MAX_PACKAGE_LEN)?;
                    self.symbol(';')?;
                }
                "option" => self.option()?,
                "message" => self.open_message()?,
                "enum" => self.open_enum()?,
                _ => return Err(self.unexpected()),
            },
            _ => return Err(self.unexpected()),
        }
        Ok(())
    }

    fn message_statement(&mut self) -> Result<(), SchemaError> {
        match self.peek() {
            Some(Token::Symbol(';')) => self.at += 1,
            Some(Token::Symbol('}')) => self.close(),
            Some(Token::Ident(word)) => match word.as_str() {
                "reserved" => self.reserved()?,
                "oneof" => {
                    self.at += 1;
                    self.ident()?;
                    self.symbol('{')?;
                    self.frames.push(Frame::Oneof);
                    self.check_depth(0)?;
                }
                "extend" => return Err(invalid("extend is not supported")),
                "message" => self.open_message()?,
                "enum" => self.open_enum()?,
                "extensions" => return Err(invalid("extension ranges are not supported")),
                "option" => self.option()?,
                _ => self.field(false)?,
            },
            // A type written with its leading dot starts a field too.
            Some(Token::Symbol('.')) => self.field(false)?,
            _ => return Err(self.unexpected()),
        }
        Ok(())
    }

    fn oneof_statement(&mut self) -> Result<(), SchemaError> {
        match self.peek() {
            Some(Token::Symbol(';')) => self.at += 1,
            Some(Token::Symbol('}')) => self.close(),
            Some(Token::Ident(word)) => match word.as_str() {
                "option" => self.option()?,
                "extend" => return Err(invalid("extend is not supported")),
                "extensions" => return Err(invalid("extension ranges are not supported")),
                "reserved" | "oneof" | "message" | "enum" => return Err(self.unexpected()),
                _ => self.field(true)?,
            },
            Some(Token::Symbol('.')) => self.field(true)?,
            _ => return Err(self.unexpected()),
        }
        Ok(())
    }

    fn enum_statement(&mut self) -> Result<(), SchemaError> {
        match self.peek() {
            Some(Token::Symbol(';')) => self.at += 1,
            Some(Token::Symbol('}')) => self.close(),
            Some(Token::Ident(word)) => match word.as_str() {
                "reserved" => self.reserved()?,
                "option" => self.option()?,
                _ => self.enum_value()?,
            },
            _ => return Err(self.unexpected()),
        }
        Ok(())
    }

    /// `NAME = [-]number [ [options] ]`. The parser does not ask for a `;`
    /// after it: the next value may follow at once.
    fn enum_value(&mut self) -> Result<(), SchemaError> {
        self.enum_values += 1;
        if self.enum_values > MAX_ENUM_VALUES_PER_ENUM {
            return Err(invalid(format!(
                "enum '{}' has more than {MAX_ENUM_VALUES_PER_ENUM} values",
                self.enum_name
            )));
        }
        self.total_enum_values += 1;
        if self.total_enum_values > MAX_ENUM_VALUES {
            return Err(invalid(format!(
                "the schema has more than {MAX_ENUM_VALUES} enum values"
            )));
        }
        self.at += 1;
        self.symbol('=')?;
        self.eat_symbol('-');
        if !matches!(self.peek(), Some(Token::IntLit(_))) {
            return Err(self.unexpected());
        }
        self.at += 1;
        if self.symbol_is('[') {
            self.option_list()?;
        }
        Ok(())
    }

    /// `option name = constant;`. A name with a `(` is a custom option.
    fn option(&mut self) -> Result<(), SchemaError> {
        self.at += 1;
        self.option_name()?;
        self.symbol('=')?;
        self.rest_of_statement()
    }

    /// The name of an option the parser interprets itself: one word. A dotted
    /// name makes the parser resolve an extension chain, one recursion step
    /// per part, and TentaBus has no option it needs that way.
    fn option_name(&mut self) -> Result<(), SchemaError> {
        self.ident()?;
        if self.symbol_is('.') {
            return Err(invalid(DOTTED_OPTION));
        }
        Ok(())
    }

    /// `[name = value, ...]` after a field number or an enum value number:
    /// every option name is checked, the values are skipped with the brackets
    /// matched and counted.
    fn option_list(&mut self) -> Result<(), SchemaError> {
        self.symbol('[')?;
        self.check_depth(1)?;
        loop {
            self.option_name()?;
            self.symbol('=')?;
            let mut open = 0usize;
            loop {
                match self.peek() {
                    None => return Err(invalid("a bracket is never closed")),
                    Some(Token::Symbol(',' | ']')) if open == 0 => break,
                    Some(Token::Symbol('{' | '[')) => {
                        open += 1;
                        self.check_depth(open + 1)?;
                    }
                    Some(Token::Symbol('}' | ']')) => {
                        if open == 0 {
                            return Err(self.unexpected());
                        }
                        open -= 1;
                    }
                    Some(Token::Symbol('(')) => return Err(self.unexpected()),
                    Some(Token::Symbol(';')) if open == 0 => return Err(self.unexpected()),
                    Some(_) => {}
                }
                self.at += 1;
            }
            if self.eat_symbol(']') {
                return Ok(());
            }
            self.symbol(',')?;
        }
    }

    /// `reserved 1, 3 to 5;` or `reserved "a", "b";`
    fn reserved(&mut self) -> Result<(), SchemaError> {
        self.at += 1;
        loop {
            match self.peek() {
                Some(Token::Symbol(';')) => {
                    self.at += 1;
                    return Ok(());
                }
                Some(
                    Token::Symbol(',') | Token::Ident(_) | Token::IntLit(_) | Token::StrLit(_),
                ) => self.at += 1,
                _ => return Err(self.unexpected()),
            }
        }
    }

    /// A field: `[label] type name = number [options];`. The statement keywords
    /// were told apart by the caller; what is left is a field or a group.
    fn field(&mut self, in_oneof: bool) -> Result<(), SchemaError> {
        if self.ident_is("map") {
            if in_oneof {
                return Err(self.unexpected());
            }
        } else if matches!(self.peek(), Some(Token::Ident(word)) if matches!(word.as_str(), "optional" | "required" | "repeated"))
        {
            if in_oneof {
                return Err(self.unexpected());
            }
            self.at += 1;
        }
        if self.ident_is("group") {
            return Err(invalid("groups are not supported"));
        }
        self.count_field()?;
        let is_map = self.field_type(0)?;
        let name = self.ident()?;
        if is_map {
            // The parser declares `<CamelName>Entry` next to the other types of
            // the scope, with its own camel-casing, and panics when that name
            // is taken.
            let entry = format!("{}Entry", ::protobuf_parse::camel_case(name));
            self.declare(&entry)?;
        }
        self.symbol('=')?;
        if matches!(self.peek(), Some(Token::IntLit(_))) {
            self.at += 1;
            if self.symbol_is('[') {
                self.option_list()?;
            }
        }
        self.rest_of_statement()
    }

    /// Counted before the type is read, so the later quadratic passes of the
    /// parser never see more fields than `lower` would accept.
    fn count_field(&mut self) -> Result<(), SchemaError> {
        if self.field_counts.is_empty() {
            return Err(self.unexpected());
        }
        let in_message = self.field_counts.last_mut().expect("checked above");
        *in_message += 1;
        if *in_message > MAX_FIELDS_PER_MESSAGE {
            let name = self.scope.last().copied().unwrap_or_default();
            return Err(invalid(format!(
                "message '{name}' has more than {MAX_FIELDS_PER_MESSAGE} fields"
            )));
        }
        self.fields += 1;
        if self.fields > MAX_FIELDS {
            return Err(invalid(format!(
                "the schema has more than {MAX_FIELDS} fields"
            )));
        }
        Ok(())
    }

    /// A field type; true for `map<K, V>`. Maps do not nest (protoc refuses
    /// them too), which also bounds the parser's recursion through its types.
    fn field_type(&mut self, level: usize) -> Result<bool, SchemaError> {
        if self.ident_is("map") {
            if level > 0 {
                return Err(invalid("a map key or value cannot be a map"));
            }
            self.at += 1;
            self.symbol('<')?;
            self.messages += 1;
            self.check_depth(1)?;
            self.field_type(level + 1)?;
            self.symbol(',')?;
            self.field_type(level + 1)?;
            self.symbol('>')?;
            return Ok(true);
        }
        self.eat_symbol('.');
        self.dotted_name(MAX_TYPE_REF_PARTS, MAX_TYPE_REF_LEN)?;
        Ok(false)
    }

    /// `a.b.c`, refused past `max_parts` parts or `max_len` bytes; returns
    /// the length. Checked as it grows, so a huge name costs no more than
    /// its cap.
    fn dotted_name(&mut self, max_parts: usize, max_len: usize) -> Result<usize, SchemaError> {
        let mut len = self.ident()?.len();
        let mut parts = 1usize;
        while self.eat_symbol('.') {
            len += 1 + self.ident()?.len();
            parts += 1;
            if parts > max_parts {
                return Err(invalid(format!("a name has more than {max_parts} parts")));
            }
            if len > max_len {
                return Err(invalid(format!(
                    "a name is longer than {max_len} characters"
                )));
            }
        }
        if len > max_len {
            return Err(invalid(format!(
                "a name is longer than {max_len} characters"
            )));
        }
        Ok(len)
    }

    /// Everything up to the `;` that ends the statement (consumed): option
    /// values, which may hold `{...}` and `[...]` with their own `;`, are
    /// skipped with the brackets matched and counted.
    fn rest_of_statement(&mut self) -> Result<(), SchemaError> {
        let mut open: Vec<char> = Vec::new();
        loop {
            match self.peek() {
                None if open.is_empty() && self.frames.is_empty() => return Err(self.unexpected()),
                None => return Err(invalid("a bracket is never closed")),
                Some(Token::Symbol(';')) if open.is_empty() => {
                    self.at += 1;
                    return Ok(());
                }
                Some(Token::Symbol(c @ ('{' | '['))) => {
                    open.push(*c);
                    self.check_depth(open.len())?;
                }
                Some(Token::Symbol(c @ ('}' | ']'))) => {
                    let want = if *c == '}' { '{' } else { '[' };
                    if open.pop() != Some(want) {
                        return Err(self.unexpected());
                    }
                }
                Some(Token::Symbol('(')) => return Err(self.unexpected()),
                Some(_) => {}
            }
            self.at += 1;
        }
    }
}
