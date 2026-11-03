// ===== File: processes/expression_translate.rs — JUEL/FEEL to CEL translator for the Camunda import =====
//
// Pure text-to-text translation: a hand-written lexer, a recursive-descent
// parser over a small closed subset of each language, and a CEL generator.
// A wrong translation silently changes a gateway condition, so the contract is
// "translate exactly or refuse": every construct outside the subset is a
// `TranslationRefusal` naming the construct and its position, never a guess.
// Semantic differences that survive translation are reported as warnings and
// never repaired with generated casts.
//
// Variables: a free identifier `x` is a process variable and becomes
// `vars.x` (or `vars["x"]` when the name is not a plain CEL identifier or is a
// CEL reserved word), matching the `vars` binding of `flow_engine::expr`.
//
// FEEL: `=` in a boolean context is equality, not assignment, so `= a = b`
// strips the leading marker and translates the rest as `a == b`.
//
// Hostile input: the source is bounded by `MAX_EXPR_CHARS` before lexing and
// the parser depth by `MAX_PARSE_DEPTH`, so no input can overflow the stack.
// The generated CEL is finally checked by `expr::validate_syntax`, which also
// enforces the CEL nesting limit.

use crate::flow_engine::expr::{self, MAX_EXPR_CHARS};

pub const EXPR_TOO_LONG: &str = "EXPR_TOO_LONG";
pub const EXPR_TOO_DEEP: &str = "EXPR_TOO_DEEP";
pub const EXPR_EMPTY: &str = "EXPR_EMPTY";
pub const EXPR_SYNTAX: &str = "EXPR_SYNTAX";
pub const EXPR_INVALID_LITERAL: &str = "EXPR_INVALID_LITERAL";
pub const EXPR_UNKNOWN_LANGUAGE: &str = "EXPR_UNKNOWN_LANGUAGE";
pub const EXPR_COMPOSITE_TEMPLATE: &str = "EXPR_COMPOSITE_TEMPLATE";
pub const EXPR_UNSUPPORTED_CONSTRUCT: &str = "EXPR_UNSUPPORTED_CONSTRUCT";
pub const EXPR_INVALID_RESULT: &str = "EXPR_INVALID_RESULT";
pub const EXPR_TYPE_COERCION: &str = "EXPR_TYPE_COERCION";
pub const EXPR_DIVISION_SEMANTICS: &str = "EXPR_DIVISION_SEMANTICS";
pub const EXPR_EMPTY_SEMANTICS: &str = "EXPR_EMPTY_SEMANTICS";

/// Bounds parser recursion independently of the 512-character cap, which still
/// admits a few hundred nested brackets or prefix operators.
const MAX_PARSE_DEPTH: usize = 64;

const CEL_RESERVED: &[&str] = &[
    "true",
    "false",
    "null",
    "in",
    "as",
    "break",
    "const",
    "continue",
    "else",
    "for",
    "function",
    "if",
    "import",
    "let",
    "loop",
    "package",
    "namespace",
    "return",
    "var",
    "void",
    "while",
];

const JUEL_WORDS: &[&str] = &[
    "and",
    "or",
    "not",
    "eq",
    "ne",
    "lt",
    "gt",
    "le",
    "ge",
    "div",
    "mod",
    "empty",
    "true",
    "false",
    "null",
    "instanceof",
];

const FEEL_WORDS: &[&str] = &[
    "if", "then", "else", "in", "and", "or", "not", "true", "false", "null", "function", "for",
    "some", "every", "between", "instance", "of", "return",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLanguage {
    Juel,
    Feel,
    Auto,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationWarning {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Translation {
    pub cel: String,
    pub warnings: Vec<TranslationWarning>,
}

/// `position` is a character index into the source passed to `translate`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {message} (at character {position})")]
pub struct TranslationRefusal {
    pub code: &'static str,
    pub message: String,
    pub position: usize,
}

fn refuse<T>(
    code: &'static str,
    message: impl Into<String>,
    position: usize,
) -> Result<T, TranslationRefusal> {
    Err(TranslationRefusal {
        code,
        message: message.into(),
        position,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Juel,
    Feel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

impl BinOp {
    fn cel(self) -> &'static str {
        match self {
            BinOp::Or => "||",
            BinOp::And => "&&",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Gt => ">",
            BinOp::Le => "<=",
            BinOp::Ge => ">=",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Mod => "%",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Expr {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Null,
    /// A process variable, whether named directly or through the execution API.
    Var(String),
    Member(Box<Expr>, String),
    Index(Box<Expr>, Box<Expr>),
    Not(Box<Expr>),
    Neg(Box<Expr>),
    Empty(Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    Range {
        subject: Box<Expr>,
        lo: Box<Expr>,
        hi: Box<Expr>,
        lo_inclusive: bool,
        hi_inclusive: bool,
    },
    InList(Box<Expr>, Vec<Expr>),
}

pub(crate) struct Parsed {
    pub expr: Expr,
    language: Lang,
}

/// Translates a JUEL or FEEL expression to a CEL expression over `vars`.
pub fn translate(
    source: &str,
    language: SourceLanguage,
) -> Result<Translation, TranslationRefusal> {
    let parsed = parse(source, language)?;
    let mut generator = Generator {
        language: parsed.language,
        warnings: Vec::new(),
    };
    let (cel, _) = generator.generate(&parsed.expr, true);
    if cel.chars().count() > MAX_EXPR_CHARS {
        return refuse(
            EXPR_TOO_LONG,
            format!("the translated expression exceeds the {MAX_EXPR_CHARS}-character CEL limit"),
            0,
        );
    }
    if let Err(error) = expr::validate_syntax(&cel, None) {
        return refuse(
            EXPR_INVALID_RESULT,
            format!(
                "the translated expression is not accepted as CEL: {}",
                error.cause
            ),
            0,
        );
    }
    Ok(Translation {
        cel,
        warnings: generator.warnings,
    })
}

pub(crate) fn parse(source: &str, language: SourceLanguage) -> Result<Parsed, TranslationRefusal> {
    if source.chars().count() > MAX_EXPR_CHARS {
        return refuse(
            EXPR_TOO_LONG,
            format!("the expression exceeds the {MAX_EXPR_CHARS}-character limit"),
            MAX_EXPR_CHARS,
        );
    }
    let chars: Vec<char> = source.chars().collect();
    let start = chars.iter().position(|c| !c.is_whitespace());
    let Some(start) = start else {
        return refuse(EXPR_EMPTY, "the expression is empty", 0);
    };
    let end = chars
        .iter()
        .rposition(|c| !c.is_whitespace())
        .map_or(start, |i| i + 1);
    let juel_wrapper = chars[start..end].len() >= 2
        && matches!(chars[start], '$' | '#')
        && chars[start + 1] == '{';
    let resolved = match language {
        SourceLanguage::Juel => Lang::Juel,
        SourceLanguage::Feel => Lang::Feel,
        SourceLanguage::Auto if juel_wrapper => Lang::Juel,
        SourceLanguage::Auto if chars[start] == '=' => Lang::Feel,
        SourceLanguage::Auto => {
            return match find_wrapper(&chars, start, end) {
                Some(at) => refuse(EXPR_COMPOSITE_TEMPLATE, "text mixed with ${...} segments is a composite template, which is not translated", at),
                None => refuse(EXPR_UNKNOWN_LANGUAGE, "the language is neither JUEL (${...} or #{...}) nor FEEL (leading =)", start),
            };
        }
    };
    match resolved {
        Lang::Juel => {
            if !juel_wrapper {
                return match find_wrapper(&chars, start, end) {
                    Some(at) => refuse(EXPR_COMPOSITE_TEMPLATE, "text mixed with ${...} segments is a composite template, which is not translated", at),
                    None => refuse(EXPR_UNSUPPORTED_CONSTRUCT, "a JUEL expression must be wrapped in ${...} or #{...}; plain text is a literal, not an expression", start),
                };
            }
            if chars[end - 1] != '}' || end - start < 3 {
                return if chars[start..end].contains(&'}') {
                    refuse(EXPR_COMPOSITE_TEMPLATE, "text after the closing } makes this a composite template, which is not translated", start)
                } else {
                    refuse(
                        EXPR_SYNTAX,
                        "the closing } of the expression is missing",
                        end,
                    )
                };
            }
            let tokens = lex(&chars, start + 2, end - 1, Lang::Juel)?;
            let expr = Parser::new(tokens, end - 1, Lang::Juel).parse_all()?;
            Ok(Parsed {
                expr,
                language: Lang::Juel,
            })
        }
        Lang::Feel => {
            let body_start = if chars[start] == '=' {
                start + 1
            } else {
                start
            };
            let tokens = lex(&chars, body_start, end, Lang::Feel)?;
            let expr = Parser::new(tokens, end, Lang::Feel).parse_all()?;
            Ok(Parsed {
                expr,
                language: Lang::Feel,
            })
        }
    }
}

fn find_wrapper(chars: &[char], start: usize, end: usize) -> Option<usize> {
    (start..end.saturating_sub(1)).find(|&i| matches!(chars[i], '$' | '#') && chars[i + 1] == '{')
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Int(i64),
    Float(f64),
    Str(String),
    Word(String),
    Sym(&'static str),
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    pos: usize,
}

const TWO_CHAR_SYMBOLS: &[&str] = &["&&", "||", "==", "!=", "<=", ">=", "->", ".."];
const ONE_CHAR_SYMBOLS: &[&str] = &[
    "+", "-", "*", "/", "%", "(", ")", "[", "]", "{", "}", ",", ".", "?", ":", ";", "!", "<", ">",
    "=",
];

fn is_word_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

fn is_word_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn lex(
    chars: &[char],
    start: usize,
    end: usize,
    lang: Lang,
) -> Result<Vec<Token>, TranslationRefusal> {
    let mut tokens = Vec::new();
    let mut i = start;
    while i < end {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let (tok, next) = lex_number(chars, i, end)?;
            tokens.push(Token { tok, pos: i });
            i = next;
        } else if c == '"' || (c == '\'' && lang == Lang::Juel) {
            let (text, next) = lex_string(chars, i, end, lang)?;
            tokens.push(Token {
                tok: Tok::Str(text),
                pos: i,
            });
            i = next;
        } else if matches!(c, '$' | '#') && i + 1 < end && chars[i + 1] == '{' {
            return refuse(EXPR_COMPOSITE_TEMPLATE, "a nested or repeated ${...} segment is a composite template, which is not translated", i);
        } else if is_word_start(c) {
            let mut j = i + 1;
            while j < end && is_word_part(chars[j]) {
                j += 1;
            }
            tokens.push(Token {
                tok: Tok::Word(chars[i..j].iter().collect()),
                pos: i,
            });
            i = j;
        } else {
            let pair: String = chars[i..end.min(i + 2)].iter().collect();
            if let Some(sym) = TWO_CHAR_SYMBOLS.iter().find(|s| **s == pair) {
                tokens.push(Token {
                    tok: Tok::Sym(sym),
                    pos: i,
                });
                i += 2;
            } else if let Some(sym) = ONE_CHAR_SYMBOLS.iter().find(|s| s.starts_with(c)) {
                tokens.push(Token {
                    tok: Tok::Sym(sym),
                    pos: i,
                });
                i += 1;
            } else {
                return refuse(EXPR_SYNTAX, format!("unexpected character `{c}`"), i);
            }
        }
    }
    Ok(tokens)
}

fn lex_number(
    chars: &[char],
    start: usize,
    end: usize,
) -> Result<(Tok, usize), TranslationRefusal> {
    let digits_from = |mut i: usize| {
        while i < end && chars[i].is_ascii_digit() {
            i += 1;
        }
        i
    };
    let mut i = digits_from(start);
    let mut is_float = false;
    if i + 1 < end && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
        is_float = true;
        i = digits_from(i + 1);
    }
    if i < end && matches!(chars[i], 'e' | 'E') {
        let sign = usize::from(i + 1 < end && matches!(chars[i + 1], '+' | '-'));
        if i + 1 + sign < end && chars[i + 1 + sign].is_ascii_digit() {
            is_float = true;
            i = digits_from(i + 1 + sign);
        }
    }
    if i < end && is_word_part(chars[i]) {
        return refuse(EXPR_SYNTAX, "a number is directly followed by letters", i);
    }
    let text: String = chars[start..i].iter().collect();
    if is_float {
        match text.parse::<f64>() {
            Ok(value) if value.is_finite() => Ok((Tok::Float(value), i)),
            _ => refuse(
                EXPR_INVALID_LITERAL,
                format!("the number `{text}` is not a finite float"),
                start,
            ),
        }
    } else {
        match text.parse::<i64>() {
            Ok(value) => Ok((Tok::Int(value), i)),
            Err(_) => refuse(
                EXPR_INVALID_LITERAL,
                format!("the integer `{text}` does not fit a 64-bit signed integer"),
                start,
            ),
        }
    }
}

fn lex_string(
    chars: &[char],
    start: usize,
    end: usize,
    lang: Lang,
) -> Result<(String, usize), TranslationRefusal> {
    let quote = chars[start];
    let mut text = String::new();
    let mut i = start + 1;
    while i < end {
        let c = chars[i];
        if c == quote {
            return Ok((text, i + 1));
        }
        if c == '\\' {
            let Some(&escaped) = chars.get(i + 1).filter(|_| i + 1 < end) else {
                break;
            };
            let value = match (escaped, lang) {
                ('\\' | '\'' | '"', _) => escaped,
                ('n', Lang::Feel) => '\n',
                ('r', Lang::Feel) => '\r',
                ('t', Lang::Feel) => '\t',
                _ => {
                    return refuse(
                        EXPR_SYNTAX,
                        format!("unsupported escape sequence `\\{escaped}`"),
                        i,
                    )
                }
            };
            text.push(value);
            i += 2;
        } else {
            text.push(c);
            i += 1;
        }
    }
    refuse(EXPR_SYNTAX, "unterminated string literal", start)
}

/// Which part of the `execution` / `variables` context API a postfix chain is
/// still standing on; only a resolved value can leave the chain.
enum Chain {
    Execution,
    Variables,
    Value(Expr),
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    depth: usize,
    end: usize,
    lang: Lang,
}

impl Parser {
    fn new(tokens: Vec<Token>, end: usize, lang: Lang) -> Self {
        Parser {
            tokens,
            at: 0,
            depth: 0,
            end,
            lang,
        }
    }

    fn parse_all(mut self) -> Result<Expr, TranslationRefusal> {
        if self.tokens.is_empty() {
            return refuse(EXPR_EMPTY, "the expression is empty", self.end);
        }
        let expr = self.expr()?;
        match self.tokens.get(self.at).cloned() {
            None => Ok(expr),
            Some(token) => self.leftover(token),
        }
    }

    fn leftover(&self, token: Token) -> Result<Expr, TranslationRefusal> {
        match (&token.tok, self.lang) {
            (Tok::Sym("}"), Lang::Juel) => refuse(EXPR_COMPOSITE_TEMPLATE, "a second ${...} segment makes this a composite template, which is not translated", token.pos),
            (Tok::Sym("="), Lang::Juel) => refuse(EXPR_UNSUPPORTED_CONSTRUCT, "assignment `=`", token.pos),
            (Tok::Sym(";"), _) => refuse(EXPR_UNSUPPORTED_CONSTRUCT, "statement sequence `;`", token.pos),
            (Tok::Word(word), Lang::Juel) if word == "instanceof" => refuse(EXPR_UNSUPPORTED_CONSTRUCT, "`instanceof` operator", token.pos),
            (Tok::Sym("->"), _) => refuse(EXPR_UNSUPPORTED_CONSTRUCT, "lambda expression `->`", token.pos),
            (Tok::Sym("+" | "-" | "*" | "/" | "%"), Lang::Feel) => refuse(EXPR_UNSUPPORTED_CONSTRUCT, "FEEL arithmetic is outside the translated subset", token.pos),
            (Tok::Word(word), Lang::Feel) => refuse(EXPR_UNSUPPORTED_CONSTRUCT, format!("`{word}`: multi-word names and FEEL functions or quantifiers are outside the translated subset"), token.pos),
            _ => refuse(EXPR_SYNTAX, format!("unexpected {}", describe(&token.tok)), token.pos),
        }
    }

    fn pos(&self) -> usize {
        self.tokens.get(self.at).map_or(self.end, |t| t.pos)
    }

    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.at).map(|t| &t.tok)
    }

    fn peek_at(&self, offset: usize) -> Option<&Tok> {
        self.tokens.get(self.at + offset).map(|t| &t.tok)
    }

    fn is_sym(&self, sym: &str) -> bool {
        matches!(self.peek(), Some(Tok::Sym(s)) if *s == sym)
    }

    fn is_word(&self, word: &str) -> bool {
        matches!(self.peek(), Some(Tok::Word(w)) if w == word)
    }

    fn expect_sym(&mut self, sym: &str) -> Result<(), TranslationRefusal> {
        if self.is_sym(sym) {
            self.at += 1;
            Ok(())
        } else {
            self.unexpected(&format!("expected `{sym}`"))
        }
    }

    fn expect_word(&mut self, word: &str) -> Result<(), TranslationRefusal> {
        if self.is_word(word) {
            self.at += 1;
            Ok(())
        } else {
            self.unexpected(&format!("expected `{word}`"))
        }
    }

    fn unexpected<T>(&self, expected: &str) -> Result<T, TranslationRefusal> {
        let found = self
            .peek()
            .map_or("end of the expression".to_owned(), describe);
        refuse(
            EXPR_SYNTAX,
            format!("{expected}, found {found}"),
            self.pos(),
        )
    }

    fn enter(&mut self) -> Result<(), TranslationRefusal> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            return refuse(
                EXPR_TOO_DEEP,
                format!("the expression is nested deeper than {MAX_PARSE_DEPTH} levels"),
                self.pos(),
            );
        }
        Ok(())
    }

    fn nested<T>(
        &mut self,
        parse: impl FnOnce(&mut Self) -> Result<T, TranslationRefusal>,
    ) -> Result<T, TranslationRefusal> {
        self.enter()?;
        let result = parse(self);
        self.depth -= 1;
        result
    }

    fn expr(&mut self) -> Result<Expr, TranslationRefusal> {
        match self.lang {
            Lang::Juel => self.nested(Self::juel_ternary),
            Lang::Feel => self.nested(Self::feel_expr),
        }
    }

    // ---- JUEL ----

    fn juel_ternary(&mut self) -> Result<Expr, TranslationRefusal> {
        let condition = self.juel_level(0)?;
        if !self.is_sym("?") {
            return Ok(condition);
        }
        self.at += 1;
        let then = self.expr()?;
        self.expect_sym(":")?;
        let otherwise = self.expr()?;
        Ok(Expr::Ternary(
            Box::new(condition),
            Box::new(then),
            Box::new(otherwise),
        ))
    }

    fn juel_op(&self, level: usize) -> Option<BinOp> {
        const LEVELS: [&[(&str, BinOp)]; 6] = [
            &[("||", BinOp::Or), ("or", BinOp::Or)],
            &[("&&", BinOp::And), ("and", BinOp::And)],
            &[
                ("==", BinOp::Eq),
                ("eq", BinOp::Eq),
                ("!=", BinOp::Ne),
                ("ne", BinOp::Ne),
            ],
            &[
                ("<", BinOp::Lt),
                ("lt", BinOp::Lt),
                (">", BinOp::Gt),
                ("gt", BinOp::Gt),
                ("<=", BinOp::Le),
                ("le", BinOp::Le),
                (">=", BinOp::Ge),
                ("ge", BinOp::Ge),
            ],
            &[("+", BinOp::Add), ("-", BinOp::Sub)],
            &[
                ("*", BinOp::Mul),
                ("/", BinOp::Div),
                ("div", BinOp::Div),
                ("%", BinOp::Mod),
                ("mod", BinOp::Mod),
            ],
        ];
        let text = match self.peek()? {
            Tok::Sym(s) => *s,
            Tok::Word(w) => w.as_str(),
            _ => return None,
        };
        LEVELS[level]
            .iter()
            .find(|(name, _)| *name == text)
            .map(|(_, op)| *op)
    }

    fn juel_level(&mut self, level: usize) -> Result<Expr, TranslationRefusal> {
        let mut left = if level == 5 {
            self.juel_unary()?
        } else {
            self.juel_level(level + 1)?
        };
        while let Some(op) = self.juel_op(level) {
            self.at += 1;
            let right = if level == 5 {
                self.juel_unary()?
            } else {
                self.juel_level(level + 1)?
            };
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn juel_unary(&mut self) -> Result<Expr, TranslationRefusal> {
        let wrap: Option<fn(Box<Expr>) -> Expr> = match self.peek() {
            Some(Tok::Sym("!")) => Some(Expr::Not),
            Some(Tok::Word(w)) if w == "not" => Some(Expr::Not),
            Some(Tok::Sym("-")) => Some(Expr::Neg),
            Some(Tok::Word(w)) if w == "empty" => Some(Expr::Empty),
            _ => None,
        };
        match wrap {
            Some(wrap) => {
                self.at += 1;
                let operand = self.nested(Self::juel_unary)?;
                Ok(wrap(Box::new(operand)))
            }
            None => self.juel_postfix(),
        }
    }

    fn juel_postfix(&mut self) -> Result<Expr, TranslationRefusal> {
        let start = self.pos();
        let mut chain = self.juel_primary()?;
        loop {
            if self.is_sym(".") {
                self.at += 1;
                let name_pos = self.pos();
                let name = match self.peek() {
                    Some(Tok::Word(w)) if JUEL_WORDS.contains(&w.as_str()) => {
                        return refuse(
                            EXPR_UNSUPPORTED_CONSTRUCT,
                            format!("reserved word `{w}` used as a property name"),
                            name_pos,
                        );
                    }
                    Some(Tok::Word(w)) => w.clone(),
                    _ => return self.unexpected("expected a property name after `.`"),
                };
                self.at += 1;
                let is_call = self.is_sym("(");
                chain = self.juel_member(chain, name, name_pos, is_call)?;
            } else if self.is_sym("[") {
                let open = self.pos();
                self.at += 1;
                let index = self.expr()?;
                self.expect_sym("]")?;
                chain = match chain {
                    Chain::Execution => {
                        return refuse(
                            EXPR_UNSUPPORTED_CONSTRUCT,
                            "indexed access on the `execution` context",
                            open,
                        )
                    }
                    Chain::Variables => match index {
                        Expr::Str(name) => Chain::Value(Expr::Var(name)),
                        _ => {
                            return refuse(
                                EXPR_UNSUPPORTED_CONSTRUCT,
                                "a variable looked up by a computed name",
                                open,
                            )
                        }
                    },
                    Chain::Value(base) => {
                        reject_literal_base(&base, open)?;
                        Chain::Value(Expr::Index(Box::new(base), Box::new(index)))
                    }
                };
            } else if self.is_sym("(") {
                let what = match &chain {
                    Chain::Value(Expr::Var(name)) => format!("function call `{name}(...)`"),
                    _ => "call".to_owned(),
                };
                return refuse(EXPR_UNSUPPORTED_CONSTRUCT, what, self.pos());
            } else {
                break;
            }
        }
        match chain {
            Chain::Value(expr) => Ok(expr),
            Chain::Execution | Chain::Variables => refuse(
                EXPR_UNSUPPORTED_CONSTRUCT,
                "the `execution` / `variables` context used as a value",
                start,
            ),
        }
    }

    fn juel_member(
        &mut self,
        chain: Chain,
        name: String,
        name_pos: usize,
        is_call: bool,
    ) -> Result<Chain, TranslationRefusal> {
        match chain {
            Chain::Execution if is_call && name == "getVariable" => {
                self.at += 1;
                let argument_pos = self.pos();
                let argument = self.expr()?;
                self.expect_sym(")")?;
                match argument {
                    Expr::Str(variable) => Ok(Chain::Value(Expr::Var(variable))),
                    _ => refuse(
                        EXPR_UNSUPPORTED_CONSTRUCT,
                        "execution.getVariable with a name that is not a string literal",
                        argument_pos,
                    ),
                }
            }
            Chain::Execution if is_call => refuse(
                EXPR_UNSUPPORTED_CONSTRUCT,
                format!("method call `execution.{name}(...)`"),
                name_pos,
            ),
            Chain::Execution if name == "variables" => Ok(Chain::Variables),
            Chain::Execution => refuse(
                EXPR_UNSUPPORTED_CONSTRUCT,
                format!("execution context property `execution.{name}`"),
                name_pos,
            ),
            Chain::Variables if is_call => refuse(
                EXPR_UNSUPPORTED_CONSTRUCT,
                format!("method call `variables.{name}(...)`"),
                name_pos,
            ),
            Chain::Variables => Ok(Chain::Value(Expr::Var(name))),
            Chain::Value(base) if is_call => refuse(
                EXPR_UNSUPPORTED_CONSTRUCT,
                format!("bean method call `{}.{name}(...)`", path_text(&base)),
                name_pos,
            ),
            Chain::Value(base) => {
                reject_literal_base(&base, name_pos)?;
                Ok(Chain::Value(Expr::Member(Box::new(base), name)))
            }
        }
    }

    fn juel_primary(&mut self) -> Result<Chain, TranslationRefusal> {
        let Some(token) = self.tokens.get(self.at).cloned() else {
            return self.unexpected("expected an operand");
        };
        match token.tok {
            Tok::Int(n) => {
                self.at += 1;
                Ok(Chain::Value(Expr::Int(n)))
            }
            Tok::Float(f) => {
                self.at += 1;
                Ok(Chain::Value(Expr::Float(f)))
            }
            Tok::Str(s) => {
                self.at += 1;
                Ok(Chain::Value(Expr::Str(s)))
            }
            Tok::Word(word) => {
                match word.as_str() {
                    "true" | "false" => {
                        self.at += 1;
                        return Ok(Chain::Value(Expr::Bool(word == "true")));
                    }
                    "null" => {
                        self.at += 1;
                        return Ok(Chain::Value(Expr::Null));
                    }
                    "instanceof" => {
                        return refuse(
                            EXPR_UNSUPPORTED_CONSTRUCT,
                            "`instanceof` operator",
                            token.pos,
                        )
                    }
                    w if JUEL_WORDS.contains(&w) => {
                        return refuse(
                            EXPR_SYNTAX,
                            format!("operator `{w}` where an operand is expected"),
                            token.pos,
                        );
                    }
                    _ => {}
                }
                if matches!(self.peek_at(1), Some(Tok::Sym(":")))
                    && matches!(self.peek_at(2), Some(Tok::Word(_)))
                    && matches!(self.peek_at(3), Some(Tok::Sym("(")))
                {
                    let Some(Tok::Word(function)) = self.peek_at(2).cloned() else {
                        unreachable!("matched above")
                    };
                    return refuse(
                        EXPR_UNSUPPORTED_CONSTRUCT,
                        format!("namespaced function `{word}:{function}(...)`"),
                        token.pos,
                    );
                }
                self.at += 1;
                Ok(match word.as_str() {
                    "execution" => Chain::Execution,
                    "variables" => Chain::Variables,
                    _ => Chain::Value(Expr::Var(word)),
                })
            }
            Tok::Sym("(") => {
                self.at += 1;
                let inner = self.expr()?;
                self.expect_sym(")")?;
                Ok(Chain::Value(inner))
            }
            Tok::Sym("[") => refuse(EXPR_UNSUPPORTED_CONSTRUCT, "list literal", token.pos),
            Tok::Sym("{") => refuse(EXPR_UNSUPPORTED_CONSTRUCT, "set or map literal", token.pos),
            Tok::Sym("}") => refuse(
                EXPR_COMPOSITE_TEMPLATE,
                "a second ${...} segment makes this a composite template, which is not translated",
                token.pos,
            ),
            other => refuse(
                EXPR_SYNTAX,
                format!(
                    "unexpected {} where an operand is expected",
                    describe(&other)
                ),
                token.pos,
            ),
        }
    }

    // ---- FEEL ----

    fn feel_expr(&mut self) -> Result<Expr, TranslationRefusal> {
        if !self.is_word("if") {
            return self.feel_or();
        }
        self.at += 1;
        let condition = self.expr()?;
        self.expect_word("then")?;
        let then = self.expr()?;
        self.expect_word("else")?;
        let otherwise = self.expr()?;
        Ok(Expr::Ternary(
            Box::new(condition),
            Box::new(then),
            Box::new(otherwise),
        ))
    }

    fn feel_or(&mut self) -> Result<Expr, TranslationRefusal> {
        let mut left = self.feel_and()?;
        while self.is_word("or") {
            self.at += 1;
            let right = self.feel_and()?;
            left = Expr::Binary(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn feel_and(&mut self) -> Result<Expr, TranslationRefusal> {
        let mut left = self.feel_comparison()?;
        while self.is_word("and") {
            self.at += 1;
            let right = self.feel_comparison()?;
            left = Expr::Binary(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn feel_comparison(&mut self) -> Result<Expr, TranslationRefusal> {
        let left = self.feel_atom()?;
        let op = match self.peek() {
            Some(Tok::Sym("=")) => BinOp::Eq,
            Some(Tok::Sym("!=")) => BinOp::Ne,
            Some(Tok::Sym("<")) => BinOp::Lt,
            Some(Tok::Sym(">")) => BinOp::Gt,
            Some(Tok::Sym("<=")) => BinOp::Le,
            Some(Tok::Sym(">=")) => BinOp::Ge,
            Some(Tok::Word(w)) if w == "in" => {
                self.at += 1;
                return self.feel_membership(left);
            }
            Some(Tok::Sym("==")) => {
                return refuse(
                    EXPR_SYNTAX,
                    "`==` is not FEEL; equality is a single `=`",
                    self.pos(),
                )
            }
            _ => return Ok(left),
        };
        self.at += 1;
        let right = self.feel_atom()?;
        Ok(Expr::Binary(op, Box::new(left), Box::new(right)))
    }

    fn feel_membership(&mut self, subject: Expr) -> Result<Expr, TranslationRefusal> {
        let open_pos = self.pos();
        let lo_inclusive = match self.peek() {
            Some(Tok::Sym("[")) => true,
            Some(Tok::Sym("(")) => false,
            _ => {
                return refuse(
                    EXPR_UNSUPPORTED_CONSTRUCT,
                    "`in` is translated only against a range or a list literal",
                    open_pos,
                )
            }
        };
        self.at += 1;
        let first = self.expr()?;
        if self.is_sym("..") {
            self.at += 1;
            let hi = self.expr()?;
            let hi_inclusive = match self.peek() {
                Some(Tok::Sym("]")) => true,
                Some(Tok::Sym(")")) => false,
                _ => return self.unexpected("expected `]` or `)` closing the range"),
            };
            self.at += 1;
            return Ok(Expr::Range {
                subject: Box::new(subject),
                lo: Box::new(first),
                hi: Box::new(hi),
                lo_inclusive,
                hi_inclusive,
            });
        }
        if !lo_inclusive {
            return refuse(
                EXPR_UNSUPPORTED_CONSTRUCT,
                "parenthesised unary test or list after `in`",
                open_pos,
            );
        }
        let mut items = vec![first];
        while self.is_sym(",") {
            self.at += 1;
            items.push(self.expr()?);
        }
        self.expect_sym("]")?;
        Ok(Expr::InList(Box::new(subject), items))
    }

    fn feel_atom(&mut self) -> Result<Expr, TranslationRefusal> {
        let Some(token) = self.tokens.get(self.at).cloned() else {
            return self.unexpected("expected an operand");
        };
        match token.tok {
            Tok::Int(n) => {
                self.at += 1;
                Ok(Expr::Int(n))
            }
            Tok::Float(f) => {
                self.at += 1;
                Ok(Expr::Float(f))
            }
            Tok::Str(s) => {
                self.at += 1;
                Ok(Expr::Str(s))
            }
            Tok::Sym("-") => {
                self.at += 1;
                match self.peek().cloned() {
                    Some(Tok::Int(n)) => {
                        self.at += 1;
                        Ok(Expr::Int(-n))
                    }
                    Some(Tok::Float(f)) => {
                        self.at += 1;
                        Ok(Expr::Float(-f))
                    }
                    _ => refuse(
                        EXPR_UNSUPPORTED_CONSTRUCT,
                        "FEEL arithmetic is outside the translated subset",
                        token.pos,
                    ),
                }
            }
            Tok::Sym("(") => {
                self.at += 1;
                let inner = self.expr()?;
                self.expect_sym(")")?;
                Ok(inner)
            }
            Tok::Sym("[") => refuse(
                EXPR_UNSUPPORTED_CONSTRUCT,
                "list literal outside of an `in` test",
                token.pos,
            ),
            Tok::Sym("{") => refuse(EXPR_UNSUPPORTED_CONSTRUCT, "context literal", token.pos),
            Tok::Word(word) => self.feel_word(word, token.pos),
            other => refuse(
                EXPR_SYNTAX,
                format!(
                    "unexpected {} where an operand is expected",
                    describe(&other)
                ),
                token.pos,
            ),
        }
    }

    fn feel_word(&mut self, word: String, pos: usize) -> Result<Expr, TranslationRefusal> {
        match word.as_str() {
            "true" | "false" => {
                self.at += 1;
                return Ok(Expr::Bool(word == "true"));
            }
            "null" => {
                self.at += 1;
                return Ok(Expr::Null);
            }
            "not" if matches!(self.peek_at(1), Some(Tok::Sym("("))) => {
                self.at += 2;
                let inner = self.expr()?;
                self.expect_sym(")")?;
                return Ok(Expr::Not(Box::new(inner)));
            }
            w if FEEL_WORDS.contains(&w) => {
                return refuse(
                    EXPR_UNSUPPORTED_CONSTRUCT,
                    format!("`{w}` is outside the translated FEEL subset"),
                    pos,
                );
            }
            _ => {}
        }
        self.at += 1;
        let mut expr = Expr::Var(word);
        loop {
            if self.is_sym(".") {
                self.at += 1;
                let name = match self.peek() {
                    Some(Tok::Word(w)) if !FEEL_WORDS.contains(&w.as_str()) => w.clone(),
                    _ => return self.unexpected("expected a path segment after `.`"),
                };
                self.at += 1;
                expr = Expr::Member(Box::new(expr), name);
            } else if self.is_sym("(") {
                return refuse(
                    EXPR_UNSUPPORTED_CONSTRUCT,
                    format!("FEEL function call on `{}`", path_text(&expr)),
                    self.pos(),
                );
            } else if self.is_sym("[") {
                return refuse(
                    EXPR_UNSUPPORTED_CONSTRUCT,
                    format!("FEEL filter or index on `{}`", path_text(&expr)),
                    self.pos(),
                );
            } else {
                return Ok(expr);
            }
        }
    }
}

fn reject_literal_base(base: &Expr, pos: usize) -> Result<(), TranslationRefusal> {
    match base {
        Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_) | Expr::Null => refuse(
            EXPR_UNSUPPORTED_CONSTRUCT,
            "member or index access on a literal",
            pos,
        ),
        _ => Ok(()),
    }
}

fn describe(tok: &Tok) -> String {
    match tok {
        Tok::Int(n) => format!("number `{n}`"),
        Tok::Float(f) => format!("number `{f}`"),
        Tok::Str(_) => "a string literal".to_owned(),
        Tok::Word(w) => format!("`{w}`"),
        Tok::Sym(s) => format!("`{s}`"),
    }
}

fn path_text(expr: &Expr) -> String {
    match expr {
        Expr::Var(name) => name.clone(),
        Expr::Member(base, name) => format!("{}.{name}", path_text(base)),
        _ => "…".to_owned(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Int,
    Float,
    Str,
    Bool,
    Null,
    Unknown,
}

impl Ty {
    fn is_numeric(self) -> bool {
        matches!(self, Ty::Int | Ty::Float)
    }
}

struct Generator {
    language: Lang,
    warnings: Vec<TranslationWarning>,
}

fn is_cel_ident(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !CEL_RESERVED.contains(&name)
}

fn select(base: &str, name: &str) -> String {
    if is_cel_ident(name) {
        format!("{base}.{name}")
    } else {
        format!("{base}[{}]", cel_string(name))
    }
}

fn cel_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn group(text: String, top: bool) -> String {
    if top {
        text
    } else {
        format!("({text})")
    }
}

impl Generator {
    fn warn(&mut self, code: &'static str, message: String) {
        let warning = TranslationWarning { code, message };
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }

    fn coercion(&mut self, what: &str) {
        let reason = match self.language {
            Lang::Juel => "JUEL coerces operand types (for example the text \"7\" to the number 7) while CEL is strict and fails on a mismatch",
            Lang::Feel => "FEEL yields null on a type mismatch while CEL fails on it",
        };
        self.warn(
            EXPR_TYPE_COERCION,
            format!("{what}: operand types are not both known to match; {reason}"),
        );
    }

    fn check_comparison(&mut self, op: &str, left: Ty, right: Ty, ordering: bool) {
        let null_equality = !ordering && (left == Ty::Null || right == Ty::Null);
        let same_known = left == right && left != Ty::Unknown && left != Ty::Null;
        let orderable = !ordering || matches!(left, Ty::Int | Ty::Float | Ty::Str);
        if !null_equality && !(same_known && orderable) {
            self.coercion(&format!("comparison `{op}`"));
        }
    }

    fn check_boolean(&mut self, op: &str, ty: Ty) {
        if !matches!(ty, Ty::Bool | Ty::Unknown) {
            self.coercion(&format!("boolean operator `{op}` on a non-boolean operand"));
        }
    }

    fn generate(&mut self, expr: &Expr, top: bool) -> (String, Ty) {
        match expr {
            Expr::Int(n) => (n.to_string(), Ty::Int),
            Expr::Float(f) => (format!("{f:?}"), Ty::Float),
            Expr::Str(s) => (cel_string(s), Ty::Str),
            Expr::Bool(b) => (b.to_string(), Ty::Bool),
            Expr::Null => ("null".to_owned(), Ty::Null),
            Expr::Var(name) => (select("vars", name), Ty::Unknown),
            Expr::Member(base, name) => {
                let (base, _) = self.generate(base, false);
                (select(&base, name), Ty::Unknown)
            }
            Expr::Index(base, key) => {
                let (base, _) = self.generate(base, false);
                match key.as_ref() {
                    Expr::Str(name) => (select(&base, name), Ty::Unknown),
                    key => {
                        let (key, _) = self.generate(key, true);
                        (format!("{base}[{key}]"), Ty::Unknown)
                    }
                }
            }
            Expr::Not(operand) => {
                let (text, ty) = self.generate(operand, false);
                self.check_boolean("!", ty);
                // cel 0.13 evaluates a bare `!!x` as a single negation, so a nested
                // negation is always parenthesised.
                let text = if text.starts_with('!') {
                    format!("({text})")
                } else {
                    text
                };
                (format!("!{text}"), Ty::Bool)
            }
            Expr::Neg(operand) => match operand.as_ref() {
                Expr::Int(n) => (format!("-{n}"), Ty::Int),
                Expr::Float(f) => (format!("-{f:?}"), Ty::Float),
                operand => {
                    let (text, ty) = self.generate(operand, false);
                    if !ty.is_numeric() {
                        self.coercion("unary minus");
                    }
                    let text = if text.starts_with('-') {
                        format!("({text})")
                    } else {
                        text
                    };
                    (
                        format!("(-{text})"),
                        if ty.is_numeric() { ty } else { Ty::Unknown },
                    )
                }
            },
            Expr::Empty(operand) => {
                let (text, _) = self.generate(operand, false);
                self.warn(
                    EXPR_EMPTY_SEMANTICS,
                    "`empty` is translated to size(...) == 0: JUEL also treats null and an unset variable as empty, CEL fails on them"
                        .to_owned(),
                );
                (format!("(size({text}) == 0)"), Ty::Bool)
            }
            Expr::Binary(op, left, right) => self.binary(*op, left, right, top),
            Expr::Ternary(condition, then, otherwise) => {
                let (condition, condition_ty) = self.generate(condition, false);
                self.check_boolean("?:", condition_ty);
                let (then, then_ty) = self.generate(then, false);
                let (otherwise, otherwise_ty) = self.generate(otherwise, false);
                let ty = if then_ty == otherwise_ty {
                    then_ty
                } else {
                    Ty::Unknown
                };
                (
                    group(format!("{condition} ? {then} : {otherwise}"), top),
                    ty,
                )
            }
            Expr::Range {
                subject,
                lo,
                hi,
                lo_inclusive,
                hi_inclusive,
            } => {
                let (subject, subject_ty) = self.generate(subject, false);
                let (lo, lo_ty) = self.generate(lo, false);
                let (hi, hi_ty) = self.generate(hi, false);
                self.check_comparison("in range", subject_ty, lo_ty, true);
                self.check_comparison("in range", subject_ty, hi_ty, true);
                let lo_op = if *lo_inclusive { ">=" } else { ">" };
                let hi_op = if *hi_inclusive { "<=" } else { "<" };
                (
                    group(
                        format!("{subject} {lo_op} {lo} && {subject} {hi_op} {hi}"),
                        top,
                    ),
                    Ty::Bool,
                )
            }
            Expr::InList(subject, items) => {
                let (subject, subject_ty) = self.generate(subject, false);
                let mut texts = Vec::with_capacity(items.len());
                for item in items {
                    let (text, ty) = self.generate(item, true);
                    self.check_comparison("in list", subject_ty, ty, false);
                    texts.push(text);
                }
                (
                    group(format!("{subject} in [{}]", texts.join(", ")), top),
                    Ty::Bool,
                )
            }
        }
    }

    fn binary(&mut self, op: BinOp, left: &Expr, right: &Expr, top: bool) -> (String, Ty) {
        let (left_text, left_ty) = self.generate(left, false);
        let (right_text, right_ty) = self.generate(right, false);
        let symbol = op.cel();
        let ty = match op {
            BinOp::Or | BinOp::And => {
                self.check_boolean(symbol, left_ty);
                self.check_boolean(symbol, right_ty);
                Ty::Bool
            }
            BinOp::Eq | BinOp::Ne => {
                self.check_comparison(symbol, left_ty, right_ty, false);
                Ty::Bool
            }
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                self.check_comparison(symbol, left_ty, right_ty, true);
                Ty::Bool
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Mod => {
                let same_numeric = left_ty == right_ty && left_ty.is_numeric();
                let float_modulo = op == BinOp::Mod && left_ty == Ty::Float;
                if !same_numeric || float_modulo {
                    let note = if op == BinOp::Add && left_ty == Ty::Str && right_ty == Ty::Str {
                        " (JUEL `+` is numeric only, CEL `+` concatenates strings)"
                    } else {
                        ""
                    };
                    self.coercion(&format!("arithmetic `{symbol}`{note}"));
                    Ty::Unknown
                } else {
                    left_ty
                }
            }
            BinOp::Div => {
                if !(left_ty == Ty::Float && right_ty == Ty::Float) {
                    self.warn(
                        EXPR_DIVISION_SEMANTICS,
                        "JUEL `/` and `div` are always floating-point division while CEL divides two integers as integers (7 / 2 is 3)".to_owned(),
                    );
                }
                if !(left_ty.is_numeric() && right_ty.is_numeric()) {
                    self.coercion("arithmetic `/`");
                }
                if left_ty == Ty::Float && right_ty == Ty::Float {
                    Ty::Float
                } else {
                    Ty::Unknown
                }
            }
        };
        (group(format!("{left_text} {symbol} {right_text}"), top), ty)
    }
}
