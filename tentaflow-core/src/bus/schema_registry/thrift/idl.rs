// =============================================================================
// File: bus/schema_registry/thrift/idl.rs — Thrift IDL lexer, parser and resolver
// =============================================================================
// Reads the supported IDL subset (see the header of the parent module) into a
// `Compiled`. Total by construction: every dimension is counted and refused
// BEFORE it allocates, nothing indexes without a bound check and nothing
// unwraps. The only recursion is the type expression (at most
// `MAX_TYPE_NESTING` levels) and the constant value (at most
// `MAX_VALUE_NESTING`), both capped by a counter that is checked on entry, so
// the native stack is never the limit.
//
// Three passes over small data:
//   1. `Lexer` cuts the text into tokens (words, numbers, strings, one-byte
//      punctuation), skips comments and picks the two `tentabus:` directives
//      out of them;
//   2. `Parser` walks the statements, fills the declaration tables and the
//      arena of type expressions, and refuses what the subset does not take;
//   3. `Parser::resolve` binds every named type to its declaration, rejects
//      typedefs that reach themselves, follows the typedefs of every
//      expression once (`Shape`) and picks the root.
// =============================================================================

use std::collections::{HashMap, HashSet};

use super::{
    invalid, Base, Compiled, DeclRef, EnumDef, Expr, ExprId, Field, Kind, Protocol, Req,
    SchemaError, Shape, StructDef, Typedef, MAX_ANNOTATION_PAIRS, MAX_ANNOTATION_PAIRS_EACH,
    MAX_CONSTS, MAX_ENUM_VALUES, MAX_ENUM_VALUES_PER_ENUM, MAX_EXPRS, MAX_FIELDS,
    MAX_FIELDS_PER_STRUCT, MAX_FIELD_ID, MAX_NAMESPACES, MAX_NAME_LEN, MAX_NAME_PARTS,
    MAX_REQUIRED_FIELDS, MAX_STRING_LITERAL, MAX_TOKENS, MAX_TYPEDEF_CHAIN, MAX_TYPES,
    MAX_TYPE_NESTING, MAX_VALUE_NESTING, MAX_VALUE_NODES, PROTOCOL_DIRECTIVE, STRUCT_DIRECTIVE,
};

type R<T> = Result<T, SchemaError>;

const BAD_CHARACTER: &str = "the schema contains a character a Thrift file cannot";
const STRING_CHARACTER: &str =
    "the schema contains a character a Thrift file cannot hold inside a string";
const STRING_OPEN: &str = "a string is never closed on its line";
const COMMENT_OPEN: &str = "a comment is never closed";

/// Words an identifier may not be: the IDL's own, so a derived text always
/// reads back the way it was written.
fn is_reserved(word: &str) -> bool {
    Base::parse(word).is_some()
        || matches!(
            word,
            "namespace"
                | "include"
                | "cpp_include"
                | "typedef"
                | "enum"
                | "senum"
                | "slist"
                | "struct"
                | "union"
                | "exception"
                | "service"
                | "extends"
                | "throws"
                | "oneway"
                | "void"
                | "required"
                | "optional"
                | "const"
                | "list"
                | "set"
                | "map"
                | "true"
                | "false"
                | "xsd_all"
                | "xsd_optional"
                | "xsd_nillable"
                | "xsd_attrs"
                | "cpp_type"
        )
}

/// C0, DEL and C1 controls, plus zero-width, bidi and other format characters.
fn is_unsafe_string_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{61C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

// =============================================================================
// Lexer
// =============================================================================

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tok<'a> {
    Word(&'a str),
    Int(i64),
    Float,
    Str,
    Punct(u8),
    Eof,
}

struct Lexer<'a> {
    text: &'a str,
    pos: usize,
    line: u32,
    tokens: usize,
    struct_directive: Option<&'a str>,
    protocol_directive: Option<Protocol>,
}

impl<'a> Lexer<'a> {
    fn new(text: &'a str) -> Lexer<'a> {
        Lexer {
            text,
            pos: 0,
            line: 1,
            tokens: 0,
            struct_directive: None,
            protocol_directive: None,
        }
    }

    fn fail<T>(&self, msg: &str) -> R<T> {
        Err(invalid(format!("{msg} (line {})", self.line)))
    }

    fn byte(&self, at: usize) -> Option<u8> {
        self.text.as_bytes().get(at).copied()
    }

    /// Blanks and comments up to the next token.
    fn skip(&mut self) -> R<()> {
        loop {
            match self.byte(self.pos) {
                Some(b'\n') => {
                    self.line += 1;
                    self.pos += 1;
                }
                Some(b' ' | b'\t' | b'\r') => self.pos += 1,
                Some(b'#') => self.line_comment(1)?,
                Some(b'/') if self.byte(self.pos + 1) == Some(b'/') => self.line_comment(2)?,
                Some(b'/') if self.byte(self.pos + 1) == Some(b'*') => {
                    let body = self.pos + 2;
                    let Some(close) = self.text.get(body..).and_then(|rest| rest.find("*/")) else {
                        return self.fail(COMMENT_OPEN);
                    };
                    let end = body + close + 2;
                    let lines = self
                        .text
                        .get(self.pos..end)
                        .unwrap_or_default()
                        .bytes()
                        .filter(|b| *b == b'\n')
                        .count();
                    self.line = self.line.saturating_add(lines as u32);
                    self.pos = end;
                }
                _ => return Ok(()),
            }
        }
    }

    /// A comment up to the end of the line; `//` and `#` comments may carry a
    /// directive.
    fn line_comment(&mut self, opener: usize) -> R<()> {
        let from = self.pos + opener;
        let end = self
            .text
            .as_bytes()
            .get(from..)
            .and_then(|rest| rest.iter().position(|b| *b == b'\n'))
            .map_or(self.text.len(), |p| from + p);
        // `from` follows an ASCII opener and `end` is a newline or the end of
        // the text: both are character boundaries.
        let comment = self.text.get(from..end).unwrap_or("");
        self.directive(comment)?;
        self.pos = end;
        Ok(())
    }

    fn directive(&mut self, comment: &'a str) -> R<()> {
        let comment = comment.trim();
        let rest_of = |prefix: &str| {
            let rest = comment.strip_prefix(prefix)?;
            (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
        };
        if let Some(name) = rest_of(STRUCT_DIRECTIVE) {
            let is_name = !name.is_empty()
                && name.len() <= MAX_NAME_LEN
                && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                && !name.as_bytes()[0].is_ascii_digit();
            if !is_name {
                return self.fail(&format!(
                    "the comment '// {STRUCT_DIRECTIVE}' must be followed by the name of a struct, union or exception"
                ));
            }
            if self.struct_directive.replace(name).is_some() {
                return self.fail(&format!(
                    "the comment '// {STRUCT_DIRECTIVE}' appears more than once"
                ));
            }
        } else if let Some(value) = rest_of(PROTOCOL_DIRECTIVE) {
            let protocol = match value {
                "binary" => Protocol::Binary,
                "compact" => Protocol::Compact,
                _ => {
                    return self.fail(&format!(
                        "the comment '// {PROTOCOL_DIRECTIVE}' must be followed by 'binary' or 'compact'"
                    ));
                }
            };
            if self.protocol_directive.replace(protocol).is_some() {
                return self.fail(&format!(
                    "the comment '// {PROTOCOL_DIRECTIVE}' appears more than once"
                ));
            }
        }
        Ok(())
    }

    /// The next token and the line it starts on.
    fn next(&mut self) -> R<(Tok<'a>, u32)> {
        self.skip()?;
        let line = self.line;
        let Some(c) = self.byte(self.pos) else {
            return Ok((Tok::Eof, line));
        };
        self.tokens += 1;
        if self.tokens > MAX_TOKENS {
            return self.fail(&format!("the schema has more than {MAX_TOKENS} tokens"));
        }
        let tok = match c {
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.word()?,
            b'0'..=b'9' => self.number()?,
            b'+' | b'-' if self.signed_number_follows() => self.number()?,
            b'.' if self.byte(self.pos + 1).is_some_and(|n| n.is_ascii_digit()) => self.number()?,
            b'"' | b'\'' => self.string(c)?,
            b'{' | b'}' | b'(' | b')' | b'[' | b']' | b'<' | b'>' | b',' | b';' | b'=' | b':'
            | b'*' => {
                self.pos += 1;
                Tok::Punct(c)
            }
            _ => return self.fail(BAD_CHARACTER),
        };
        Ok((tok, line))
    }

    /// A `+` or `-` that starts a number: digits, or a dot and digits, follow.
    fn signed_number_follows(&self) -> bool {
        match self.byte(self.pos + 1) {
            Some(b) if b.is_ascii_digit() => true,
            Some(b'.') => self.byte(self.pos + 2).is_some_and(|b| b.is_ascii_digit()),
            _ => false,
        }
    }

    fn word(&mut self) -> R<Tok<'a>> {
        let start = self.pos;
        while self
            .byte(self.pos)
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
        {
            self.pos += 1;
        }
        // Only ASCII bytes were consumed, so both ends are boundaries.
        let word = self.text.get(start..self.pos).unwrap_or("");
        if word.len() > MAX_NAME_LEN {
            return self.fail(&format!("a name is longer than {MAX_NAME_LEN} characters"));
        }
        let parts = word.split('.');
        if word.ends_with('.') || parts.clone().any(str::is_empty) {
            return self.fail("a name has an empty part");
        }
        if parts.count() > MAX_NAME_PARTS {
            return self.fail(&format!("a name has more than {MAX_NAME_PARTS} parts"));
        }
        Ok(Tok::Word(word))
    }

    fn number(&mut self) -> R<Tok<'a>> {
        let start = self.pos;
        let negative = self.byte(self.pos) == Some(b'-');
        if matches!(self.byte(self.pos), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        let digits = |lexer: &Lexer, at: usize, pred: fn(&u8) -> bool| {
            lexer
                .text
                .as_bytes()
                .get(at..)
                .unwrap_or_default()
                .iter()
                .take_while(|b| pred(b))
                .count()
        };
        let mut float = false;
        let hex = self.byte(self.pos) == Some(b'0')
            && matches!(self.byte(self.pos + 1), Some(b'x' | b'X'));
        if hex {
            self.pos += 2;
            let n = digits(self, self.pos, u8::is_ascii_hexdigit);
            if n == 0 || n > 16 {
                return self.fail("an integer literal is malformed or out of range");
            }
            self.pos += n;
        } else {
            self.pos += digits(self, self.pos, u8::is_ascii_digit);
            if self.byte(self.pos) == Some(b'.')
                && self.byte(self.pos + 1).is_some_and(|b| b.is_ascii_digit())
            {
                float = true;
                self.pos += 1;
                self.pos += digits(self, self.pos, u8::is_ascii_digit);
            }
            if matches!(self.byte(self.pos), Some(b'e' | b'E')) {
                let sign = usize::from(matches!(self.byte(self.pos + 1), Some(b'+' | b'-')));
                if self
                    .byte(self.pos + 1 + sign)
                    .is_some_and(|b| b.is_ascii_digit())
                {
                    float = true;
                    self.pos += 1 + sign;
                    self.pos += digits(self, self.pos, u8::is_ascii_digit);
                }
            }
        }
        if self
            .byte(self.pos)
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
        {
            return self.fail("a number is malformed");
        }
        if self.pos - start > 32 {
            return self.fail("a number is longer than 32 characters");
        }
        if float {
            return Ok(Tok::Float);
        }
        let literal = self.text.get(start..self.pos).unwrap_or("");
        let body = literal.trim_start_matches(['+', '-']);
        let parsed = match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            Some(hex) => i64::from_str_radix(hex, 16),
            None => body.parse::<i64>(),
        };
        match parsed {
            Ok(v) => Ok(Tok::Int(if negative { -v } else { v })),
            Err(_) => self.fail("an integer literal is out of range"),
        }
    }

    fn string(&mut self, quote: u8) -> R<Tok<'a>> {
        self.pos += 1;
        let mut size = 0usize;
        let mut chars = self.text.get(self.pos..).unwrap_or("").chars();
        loop {
            let Some(c) = chars.next() else {
                return self.fail(STRING_OPEN);
            };
            self.pos += c.len_utf8();
            if c == char::from(quote) {
                return Ok(Tok::Str);
            }
            if c == '\n' || c == '\r' {
                return self.fail(STRING_OPEN);
            }
            if c == '\\' {
                let Some(escaped) = chars.next() else {
                    return self.fail(STRING_OPEN);
                };
                self.pos += escaped.len_utf8();
                if !matches!(escaped, '\\' | '"' | '\'' | 'n' | 'r' | 't') {
                    return self.fail("a string holds an escape sequence Thrift does not define");
                }
                size += 1;
            } else if is_unsafe_string_char(c) {
                return self.fail(STRING_CHARACTER);
            } else {
                size += c.len_utf8();
            }
            if size > MAX_STRING_LITERAL {
                return self.fail(&format!(
                    "a string is longer than {MAX_STRING_LITERAL} bytes"
                ));
            }
        }
    }
}

// =============================================================================
// Parser
// =============================================================================

struct Parser<'a> {
    lex: Lexer<'a>,
    tok: Tok<'a>,
    line: u32,
    out: Compiled,
    types: HashMap<&'a str, DeclRef>,
    consts: HashSet<&'a str>,
    namespaces: usize,
    annotation_pairs: usize,
    value_nodes: usize,
    fields_total: usize,
    enum_values_total: usize,
}

pub(super) fn parse(text: &str) -> R<Compiled> {
    let mut parser = Parser {
        lex: Lexer::new(text),
        tok: Tok::Eof,
        line: 1,
        out: Compiled {
            structs: Vec::new(),
            enums: Vec::new(),
            typedefs: Vec::new(),
            exprs: Vec::new(),
            shapes: Vec::new(),
            order: Vec::new(),
            root: 0,
            protocol: Protocol::Binary,
        },
        types: HashMap::new(),
        consts: HashSet::new(),
        namespaces: 0,
        annotation_pairs: 0,
        value_nodes: 0,
        fields_total: 0,
        enum_values_total: 0,
    };
    parser.advance()?;
    parser.file()?;
    parser.resolve()
}

impl<'a> Parser<'a> {
    fn advance(&mut self) -> R<()> {
        let (tok, line) = self.lex.next()?;
        self.tok = tok;
        self.line = line;
        Ok(())
    }

    fn fail<T>(&self, msg: impl AsRef<str>) -> R<T> {
        Err(invalid(format!("{} (line {})", msg.as_ref(), self.line)))
    }

    /// The current token in words that cannot carry anything but the schema's
    /// own punctuation and identifiers.
    fn describe(&self) -> String {
        match self.tok {
            Tok::Word(w) => format!("the word '{w}'"),
            Tok::Int(_) | Tok::Float => "a number".to_string(),
            Tok::Str => "a string".to_string(),
            Tok::Punct(c) => format!("'{}'", char::from(c)),
            Tok::Eof => "the end of the schema".to_string(),
        }
    }

    fn unexpected<T>(&self, expected: &str) -> R<T> {
        self.fail(format!("expected {expected}, found {}", self.describe()))
    }

    fn punct(&mut self, c: u8) -> R<()> {
        if self.tok == Tok::Punct(c) {
            self.advance()
        } else {
            self.unexpected(&format!("'{}'", char::from(c)))
        }
    }

    fn eat(&mut self, c: u8) -> R<bool> {
        if self.tok == Tok::Punct(c) {
            self.advance()?;
            return Ok(true);
        }
        Ok(false)
    }

    /// `,` or `;` between members, optional.
    fn separator(&mut self) -> R<()> {
        if !self.eat(b',')? {
            self.eat(b';')?;
        }
        Ok(())
    }

    /// A name a declaration introduces.
    fn declared_name(&mut self, what: &str) -> R<&'a str> {
        let Tok::Word(w) = self.tok else {
            return self.unexpected(&format!("the name of a {what}"));
        };
        if w.contains('.') {
            return self.fail(format!("the name of a {what} cannot contain a dot"));
        }
        if is_reserved(w) {
            return self.fail(format!(
                "'{w}' is a reserved word and cannot be the name of a {what}"
            ));
        }
        self.advance()?;
        Ok(w)
    }

    fn declare_type(&mut self, name: &'a str, decl: DeclRef) -> R<()> {
        if self.types.len() >= MAX_TYPES {
            return self.fail(format!("the schema has more than {MAX_TYPES} types"));
        }
        if self.types.insert(name, decl).is_some() {
            return self.fail(format!("type '{name}' is defined more than once"));
        }
        self.out.order.push(decl);
        Ok(())
    }

    fn file(&mut self) -> R<()> {
        loop {
            let word = match self.tok {
                Tok::Eof => return Ok(()),
                Tok::Word(w) => w,
                _ => return self.unexpected("a declaration"),
            };
            match word {
                "namespace" => self.namespace()?,
                "include" | "cpp_include" => {
                    return self.fail(format!(
                        "{word} is not supported: the pattern must be one self-contained file"
                    ));
                }
                "service" | "extends" | "throws" | "oneway" | "void" => {
                    return self.fail("service is not supported: only data types are described");
                }
                "senum" | "slist" => {
                    return self.fail(format!("{word} is not supported"));
                }
                "typedef" => self.typedef()?,
                "enum" => self.enumeration()?,
                "const" => self.constant()?,
                "struct" => self.structured(Kind::Struct)?,
                "union" => self.structured(Kind::Union)?,
                "exception" => self.structured(Kind::Exception)?,
                _ => {
                    return self.fail(format!(
                        "'{word}' is not part of the supported Thrift IDL subset"
                    ));
                }
            }
        }
    }

    fn namespace(&mut self) -> R<()> {
        self.advance()?;
        self.namespaces += 1;
        if self.namespaces > MAX_NAMESPACES {
            return self.fail(format!(
                "the schema has more than {MAX_NAMESPACES} namespaces"
            ));
        }
        if !matches!(self.tok, Tok::Word(_) | Tok::Punct(b'*')) {
            return self.unexpected("a namespace scope");
        }
        self.advance()?;
        if !matches!(self.tok, Tok::Word(_)) {
            return self.unexpected("a namespace name");
        }
        self.advance()?;
        self.annotation()?;
        self.separator()
    }

    /// `( key = "value", ... )`, read and dropped.
    fn annotation(&mut self) -> R<()> {
        if !self.eat(b'(')? {
            return Ok(());
        }
        let mut pairs = 0;
        while self.tok != Tok::Punct(b')') {
            pairs += 1;
            self.annotation_pairs += 1;
            if pairs > MAX_ANNOTATION_PAIRS_EACH || self.annotation_pairs > MAX_ANNOTATION_PAIRS {
                return self.fail(format!(
                    "an annotation has more than {MAX_ANNOTATION_PAIRS_EACH} entries, or the schema more than {MAX_ANNOTATION_PAIRS}"
                ));
            }
            if !matches!(self.tok, Tok::Word(_)) {
                return self.unexpected("an annotation name");
            }
            self.advance()?;
            if self.eat(b'=')? {
                if self.tok != Tok::Str {
                    return self.unexpected("a string");
                }
                self.advance()?;
            }
            self.separator()?;
        }
        self.advance()
    }

    fn typedef(&mut self) -> R<()> {
        self.advance()?;
        let target = self.parse_type(0)?;
        let name = self.declared_name("typedef")?;
        let index = self.out.typedefs.len();
        self.declare_type(name, DeclRef::Typedef(index))?;
        self.out.typedefs.push(Typedef {
            name: name.to_string(),
            target,
        });
        self.annotation()?;
        self.separator()
    }

    fn constant(&mut self) -> R<()> {
        self.advance()?;
        if self.consts.len() >= MAX_CONSTS {
            return self.fail(format!("the schema has more than {MAX_CONSTS} constants"));
        }
        self.parse_type(0)?;
        let name = self.declared_name("constant")?;
        if !self.consts.insert(name) {
            return self.fail(format!("constant '{name}' is defined more than once"));
        }
        self.punct(b'=')?;
        self.value(0)?;
        self.annotation()?;
        self.separator()
    }

    fn enumeration(&mut self) -> R<()> {
        self.advance()?;
        let name = self.declared_name("enum")?;
        let index = self.out.enums.len();
        self.declare_type(name, DeclRef::Enum(index))?;
        self.punct(b'{')?;
        let mut values: Vec<(String, i32)> = Vec::new();
        let mut names: HashSet<&str> = HashSet::new();
        let mut next: i64 = 0;
        while self.tok != Tok::Punct(b'}') {
            if values.len() >= MAX_ENUM_VALUES_PER_ENUM {
                return self.fail(format!(
                    "enum '{name}' has more than {MAX_ENUM_VALUES_PER_ENUM} values"
                ));
            }
            if self.enum_values_total >= MAX_ENUM_VALUES {
                return self.fail(format!(
                    "the schema has more than {MAX_ENUM_VALUES} enum values"
                ));
            }
            let value_name = self.declared_name("enum value")?;
            if !names.insert(value_name) {
                return self.fail(format!("enum '{name}' has two values named '{value_name}'"));
            }
            if self.eat(b'=')? {
                let Tok::Int(n) = self.tok else {
                    return self.unexpected("an integer");
                };
                next = n;
                self.advance()?;
            }
            let Ok(number) = i32::try_from(next) else {
                return self.fail(format!(
                    "value '{value_name}' of enum '{name}' does not fit a 32-bit integer"
                ));
            };
            self.annotation()?;
            self.separator()?;
            self.enum_values_total += 1;
            values.push((value_name.to_string(), number));
            next = i64::from(number) + 1;
        }
        self.advance()?;
        self.annotation()?;
        let mut numbers: Vec<i32> = values.iter().map(|(_, n)| *n).collect();
        numbers.sort_unstable();
        numbers.dedup();
        self.out.enums.push(EnumDef {
            name: name.to_string(),
            values,
            numbers,
        });
        Ok(())
    }

    fn structured(&mut self, kind: Kind) -> R<()> {
        self.advance()?;
        let name = self.declared_name(kind.keyword())?;
        let index = self.out.structs.len();
        self.declare_type(name, DeclRef::Struct(index))?;
        self.punct(b'{')?;
        let mut fields: Vec<Field> = Vec::new();
        let mut ids: HashSet<i16> = HashSet::new();
        let mut names: HashSet<&str> = HashSet::new();
        let mut required = 0usize;
        while self.tok != Tok::Punct(b'}') {
            if fields.len() >= MAX_FIELDS_PER_STRUCT {
                return self.fail(format!(
                    "{} '{name}' has more than {MAX_FIELDS_PER_STRUCT} fields",
                    kind.keyword()
                ));
            }
            if self.fields_total >= MAX_FIELDS {
                return self.fail(format!("the schema has more than {MAX_FIELDS} fields"));
            }
            let (field, field_name) = self.field(kind, name)?;
            if !ids.insert(field.id) {
                return self.fail(format!(
                    "{} '{name}' has two fields with the id {}",
                    kind.keyword(),
                    field.id
                ));
            }
            if !names.insert(field_name) {
                return self.fail(format!(
                    "{} '{name}' has two fields named '{}'",
                    kind.keyword(),
                    field.name
                ));
            }
            self.fields_total += 1;
            let mut field = field;
            if field.req == Req::Required {
                if required >= MAX_REQUIRED_FIELDS {
                    return self.fail(format!(
                        "{} '{name}' has more than {MAX_REQUIRED_FIELDS} required fields",
                        kind.keyword()
                    ));
                }
                field.required_bit = Some(required as u32);
                required += 1;
            }
            fields.push(field);
        }
        self.advance()?;
        self.annotation()?;
        let mut by_id: Vec<(i16, usize)> =
            fields.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
        by_id.sort_unstable();
        let required_mask = fields
            .iter()
            .filter_map(|f| f.required_bit)
            .fold(0u64, |mask, bit| mask | (1u64 << bit));
        self.out.structs.push(StructDef {
            name: name.to_string(),
            kind,
            fields,
            by_id,
            required_mask,
        });
        Ok(())
    }

    fn field(&mut self, kind: Kind, owner: &str) -> R<(Field, &'a str)> {
        let id = if let Tok::Int(n) = self.tok {
            self.advance()?;
            self.punct(b':')?;
            Some(n)
        } else {
            None
        };
        let mut req = Req::Default;
        if let Tok::Word(w @ ("required" | "optional")) = self.tok {
            req = if w == "required" {
                Req::Required
            } else {
                Req::Optional
            };
            if kind == Kind::Union && req == Req::Required {
                return self.fail(format!("a member of union '{owner}' cannot be required"));
            }
            self.advance()?;
        }
        let ty = self.parse_type(0)?;
        let name = self.declared_name("field")?;
        if self.eat(b'=')? {
            self.value(0)?;
        }
        self.annotation()?;
        self.separator()?;
        let Some(id) = id else {
            return self.fail(format!(
                "field '{name}' of {} '{owner}' has no explicit id: write the id out ('1: string {name}'), implicit ids are not supported",
                kind.keyword()
            ));
        };
        if !(1..=MAX_FIELD_ID).contains(&id) {
            return self.fail(format!(
                "field '{name}' of {} '{owner}' has an id outside 1 to {MAX_FIELD_ID}",
                kind.keyword()
            ));
        }
        let field = Field {
            id: id as i16,
            name: name.to_string(),
            req,
            ty,
            required_bit: None,
        };
        Ok((field, name))
    }

    fn push_expr(&mut self, expr: Expr) -> R<ExprId> {
        if self.out.exprs.len() >= MAX_EXPRS {
            return self.fail(format!(
                "the schema has more than {MAX_EXPRS} type expressions"
            ));
        }
        self.out.exprs.push(expr);
        Ok(self.out.exprs.len() - 1)
    }

    /// A type; `depth` counts the containers around it.
    fn parse_type(&mut self, depth: usize) -> R<ExprId> {
        let Tok::Word(word) = self.tok else {
            return self.unexpected("a type");
        };
        let expr = match word {
            "list" | "set" | "map" => {
                if depth >= MAX_TYPE_NESTING {
                    return self.fail(format!(
                        "types are nested deeper than {MAX_TYPE_NESTING} levels"
                    ));
                }
                self.advance()?;
                self.punct(b'<')?;
                let first = self.parse_type(depth + 1)?;
                let expr = match word {
                    "list" => Expr::List(first),
                    "set" => Expr::Set(first),
                    _ => {
                        self.punct(b',')?;
                        Expr::Map(first, self.parse_type(depth + 1)?)
                    }
                };
                if self.tok != Tok::Punct(b'>') {
                    return self.unexpected("'>'");
                }
                expr
            }
            "slist" | "senum" => return self.fail(format!("{word} is not supported")),
            _ => {
                if let Some(base) = Base::parse(word) {
                    Expr::Base(base)
                } else if is_reserved(word) {
                    return self.unexpected("a type");
                } else if word.contains('.') {
                    return self.fail(
                        "a type name with a dot points into an included file, which is not supported",
                    );
                } else {
                    Expr::Ref(word.to_string())
                }
            }
        };
        self.advance()?;
        self.annotation()?;
        self.push_expr(expr)
    }

    /// A constant or default value, read for its syntax and dropped.
    fn value(&mut self, depth: usize) -> R<()> {
        self.value_nodes += 1;
        if self.value_nodes > MAX_VALUE_NODES {
            return self.fail(format!(
                "the constants and default values have more than {MAX_VALUE_NODES} parts"
            ));
        }
        match self.tok {
            Tok::Int(_) | Tok::Float | Tok::Str | Tok::Word(_) => self.advance(),
            Tok::Punct(open @ (b'[' | b'{')) => {
                if depth >= MAX_VALUE_NESTING {
                    return self.fail(format!(
                        "a constant is nested deeper than {MAX_VALUE_NESTING} levels"
                    ));
                }
                self.advance()?;
                let close = if open == b'[' { b']' } else { b'}' };
                while self.tok != Tok::Punct(close) {
                    self.value(depth + 1)?;
                    if open == b'{' {
                        self.punct(b':')?;
                        self.value(depth + 1)?;
                    }
                    self.separator()?;
                }
                self.advance()
            }
            _ => self.unexpected("a value"),
        }
    }
}

// =============================================================================
// Resolution
// =============================================================================

/// Children of an expression in the graph the cycle search walks: the
/// elements of a container, the target of a typedef.
fn edges(c: &Compiled, node: ExprId) -> ([ExprId; 2], usize) {
    match c.exprs.get(node) {
        Some(Expr::List(child) | Expr::Set(child)) => ([*child, 0], 1),
        Some(Expr::Map(k, v)) => ([*k, *v], 2),
        Some(Expr::Typedef(t)) => ([c.typedefs.get(*t).map_or(node, |d| d.target), 0], 1),
        _ => ([0, 0], 0),
    }
}

/// A typedef that reaches itself, directly or through containers
/// (`typedef list<A> A`), has no finite shape. Iterative: the schema decides
/// how long the path is.
fn check_no_typedef_cycle(c: &Compiled) -> R<()> {
    // 0 = not seen, 1 = on the path, 2 = done.
    let mut state = vec![0u8; c.exprs.len()];
    let mut path: Vec<(ExprId, usize)> = Vec::new();
    for start in 0..c.exprs.len() {
        if state[start] != 0 {
            continue;
        }
        state[start] = 1;
        path.push((start, 0));
        while let Some(&(node, next_edge)) = path.last() {
            let (children, count) = edges(c, node);
            if next_edge >= count {
                state[node] = 2;
                path.pop();
                continue;
            }
            if let Some(top) = path.last_mut() {
                top.1 += 1;
            }
            let child = children[next_edge];
            match state.get(child).copied().unwrap_or(2) {
                0 => {
                    state[child] = 1;
                    path.push((child, 0));
                }
                1 => {
                    let named = path
                        .iter()
                        .skip_while(|(n, _)| *n != child)
                        .find_map(|(n, _)| match c.exprs.get(*n) {
                            Some(Expr::Typedef(t)) => c.typedefs.get(*t).map(|d| d.name.as_str()),
                            _ => None,
                        });
                    return Err(invalid(match named {
                        Some(name) => format!("typedef '{name}' refers to itself"),
                        None => "a typedef refers to itself".to_string(),
                    }));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Whether the root can be written at all: a `required` field of a type that
/// has no value (a struct that requires itself, an enum without values)
/// leaves the schema with no valid payload.
fn root_is_inhabited(c: &Compiled) -> bool {
    let mut inhabited = vec![false; c.structs.len()];
    loop {
        let mut changed = false;
        for (i, def) in c.structs.iter().enumerate() {
            if inhabited[i] {
                continue;
            }
            let writable = def
                .fields
                .iter()
                .filter(|f| f.req == Req::Required)
                .all(|f| match c.shapes.get(f.ty) {
                    Some(Shape::Enum(e)) => c.enums.get(*e).is_some_and(|d| !d.values.is_empty()),
                    Some(Shape::Struct(s)) => inhabited.get(*s).copied().unwrap_or(false),
                    _ => true,
                });
            if writable {
                inhabited[i] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    inhabited.get(c.root).copied().unwrap_or(false)
}

impl Parser<'_> {
    fn resolve(self) -> R<Compiled> {
        let struct_directive = self.lex.struct_directive;
        let mut c = self.out;
        c.protocol = self.lex.protocol_directive.unwrap_or(Protocol::Binary);

        for e in 0..c.exprs.len() {
            let resolved = match &c.exprs[e] {
                Expr::Ref(name) => match self.types.get(name.as_str()) {
                    Some(DeclRef::Struct(i)) => Expr::Struct(*i),
                    Some(DeclRef::Enum(i)) => Expr::Enum(*i),
                    Some(DeclRef::Typedef(i)) => Expr::Typedef(*i),
                    None => {
                        return Err(invalid(format!(
                            "type '{name}' is not defined in the schema"
                        )));
                    }
                },
                _ => continue,
            };
            c.exprs[e] = resolved;
        }
        check_no_typedef_cycle(&c)?;

        let mut shapes = Vec::with_capacity(c.exprs.len());
        for start in 0..c.exprs.len() {
            let (mut at, mut hops) = (start, 0usize);
            let shape = loop {
                match c.exprs.get(at) {
                    Some(Expr::Base(b)) => break Shape::Base(*b),
                    Some(Expr::Struct(s)) => break Shape::Struct(*s),
                    Some(Expr::Enum(i)) => break Shape::Enum(*i),
                    Some(Expr::List(child)) => break Shape::List(*child),
                    Some(Expr::Set(child)) => break Shape::Set(*child),
                    Some(Expr::Map(k, v)) => break Shape::Map(*k, *v),
                    Some(Expr::Typedef(t)) => {
                        hops += 1;
                        if hops > MAX_TYPEDEF_CHAIN {
                            return Err(invalid(format!(
                                "a chain of typedefs is longer than {MAX_TYPEDEF_CHAIN}"
                            )));
                        }
                        match c.typedefs.get(*t) {
                            Some(def) => at = def.target,
                            None => return Err(invalid("a typedef could not be resolved")),
                        }
                    }
                    Some(Expr::Ref(_)) | None => {
                        return Err(invalid("a type could not be resolved"));
                    }
                }
            };
            shapes.push(shape);
        }
        c.shapes = shapes;

        c.root = match struct_directive {
            Some(name) => match self.types.get(name) {
                Some(DeclRef::Struct(i)) => *i,
                _ => {
                    return Err(invalid(format!(
                        "the designated struct '{name}' is not a top-level struct, union or exception of the schema"
                    )));
                }
            },
            None => c
                .order
                .iter()
                .find_map(|d| match d {
                    DeclRef::Struct(i) => Some(*i),
                    _ => None,
                })
                .ok_or_else(|| invalid("the schema declares no struct, union or exception"))?,
        };
        if !root_is_inhabited(&c) {
            return Err(invalid(
                "the schema can never produce a value: the root type requires itself or an enum without values",
            ));
        }
        Ok(c)
    }
}
