// ===== File: gbnf.rs — GBNF (llama.cpp-compatible EBNF) parser → grammar AST =====
// Recursive-descent parser for the GBNF subset used across FORGE: named rules
// (`name ::= ...`), string literals, `[...]` char classes (with ranges and
// negation), rule references, `(...)` groups, `.` (any-but-newline), the
// postfix operators `* + ?` and `{n}` / `{n,}` / `{n,m}`, `|` alternation and
// `#` line comments.

use forge_types::{ForgeError, Result};

use crate::builder::{AstRule, CharSet, Item};

pub fn parse(src: &str) -> Result<Vec<AstRule>> {
    Parser {
        chars: src.chars().collect(),
        pos: 0,
    }
    .parse_grammar()
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn err(&self, msg: impl Into<String>) -> ForgeError {
        ForgeError::Grammar(format!("GBNF parse error at char {}: {}", self.pos, msg.into()))
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    /// Skip spaces, tabs, newlines and `#` line comments.
    fn skip_ws(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c == ' ' || c == '\t' || c == '\r' || c == '\n' => {
                    self.pos += 1;
                }
                Some('#') => {
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        if c == '\n' {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    }

    fn parse_grammar(&mut self) -> Result<Vec<AstRule>> {
        let mut rules = Vec::new();
        self.skip_ws();
        while self.peek().is_some() {
            rules.push(self.parse_rule()?);
            self.skip_ws();
        }
        if rules.is_empty() {
            return Err(self.err("empty grammar"));
        }
        Ok(rules)
    }

    fn parse_rule(&mut self) -> Result<AstRule> {
        let name = self.parse_name()?;
        self.skip_ws();
        self.expect_str("::=")?;
        self.skip_ws();
        let alternates = self.parse_alternates()?;
        Ok(AstRule { name, alternates })
    }

    fn expect_str(&mut self, s: &str) -> Result<()> {
        for want in s.chars() {
            if self.bump() != Some(want) {
                return Err(self.err(format!("expected `{s}`")));
            }
        }
        Ok(())
    }

    fn parse_name(&mut self) -> Result<String> {
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                name.push(c);
                self.pos += 1;
            } else {
                break;
            }
        }
        if name.is_empty() {
            return Err(self.err("expected rule name"));
        }
        Ok(name)
    }

    /// One `|`-separated list of sequences. Stops at `)` (group close), a rule
    /// header (`name ::=`) or end of input.
    fn parse_alternates(&mut self) -> Result<Vec<Vec<Item>>> {
        let mut alts = vec![self.parse_sequence()?];
        loop {
            self.skip_ws_inline();
            if self.peek() == Some('|') {
                self.pos += 1;
                self.skip_ws();
                alts.push(self.parse_sequence()?);
            } else {
                break;
            }
        }
        Ok(alts)
    }

    /// Skip only spaces/tabs and comments, not newlines — a newline ends a
    /// rule's right-hand side (unless a `|` or `(` continuation follows, which
    /// callers handle by consuming full whitespace before `|`).
    fn skip_ws_inline(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c == ' ' || c == '\t' => self.pos += 1,
                Some('#') => {
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        if c == '\n' {
                            break;
                        }
                    }
                }
                Some('\r') | Some('\n') => {
                    // Peek past the newline: a `|` continuation keeps the rule
                    // going, anything else ends the sequence here.
                    let save = self.pos;
                    self.skip_ws();
                    if self.peek() == Some('|') {
                        return;
                    }
                    self.pos = save;
                    return;
                }
                _ => return,
            }
        }
    }

    fn parse_sequence(&mut self) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        loop {
            self.skip_ws_inline();
            match self.peek() {
                None | Some('|') | Some(')') => break,
                Some('\n') | Some('\r') => break,
                _ => {}
            }
            // A rule header (`name ::=`) terminates the current rule body.
            if self.at_rule_header() {
                break;
            }
            let atom = self.parse_atom()?;
            let item = self.parse_postfix(atom)?;
            items.push(item);
        }
        Ok(items)
    }

    /// Lookahead: does an identifier followed by `::=` start here?
    fn at_rule_header(&self) -> bool {
        let mut i = self.pos;
        let mut saw = false;
        while let Some(&c) = self.chars.get(i) {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                saw = true;
                i += 1;
            } else {
                break;
            }
        }
        if !saw {
            return false;
        }
        while let Some(&c) = self.chars.get(i) {
            if c == ' ' || c == '\t' {
                i += 1;
            } else {
                break;
            }
        }
        self.chars.get(i) == Some(&':')
            && self.chars.get(i + 1) == Some(&':')
            && self.chars.get(i + 2) == Some(&'=')
    }

    fn parse_atom(&mut self) -> Result<Item> {
        match self.peek() {
            Some('"') => self.parse_string(),
            Some('[') => Ok(Item::Class(self.parse_class()?)),
            Some('(') => {
                self.pos += 1;
                self.skip_ws();
                let alts = self.parse_alternates()?;
                self.skip_ws();
                if self.bump() != Some(')') {
                    return Err(self.err("expected `)`"));
                }
                Ok(Item::Group(alts))
            }
            Some('.') => {
                self.pos += 1;
                Ok(Item::Class(CharSet::any_but_newline()))
            }
            Some(c) if c.is_ascii_alphanumeric() || c == '_' => {
                let name = self.parse_name()?;
                Ok(Item::Ref(name))
            }
            other => Err(self.err(format!("unexpected {other:?}"))),
        }
    }

    fn parse_postfix(&mut self, atom: Item) -> Result<Item> {
        match self.peek() {
            Some('*') => {
                self.pos += 1;
                Ok(Item::Repeat {
                    item: Box::new(atom),
                    min: 0,
                    max: None,
                })
            }
            Some('+') => {
                self.pos += 1;
                Ok(Item::Repeat {
                    item: Box::new(atom),
                    min: 1,
                    max: None,
                })
            }
            Some('?') => {
                self.pos += 1;
                Ok(Item::Repeat {
                    item: Box::new(atom),
                    min: 0,
                    max: Some(1),
                })
            }
            Some('{') => {
                let (min, max) = self.parse_repeat_count()?;
                Ok(Item::Repeat {
                    item: Box::new(atom),
                    min,
                    max,
                })
            }
            _ => Ok(atom),
        }
    }

    fn parse_repeat_count(&mut self) -> Result<(u32, Option<u32>)> {
        self.expect_str("{")?;
        let min = self.parse_number()?;
        let max = if self.peek() == Some(',') {
            self.pos += 1;
            if self.peek() == Some('}') {
                None
            } else {
                Some(self.parse_number()?)
            }
        } else {
            Some(min)
        };
        if self.bump() != Some('}') {
            return Err(self.err("expected `}` in repeat count"));
        }
        Ok((min, max))
    }

    fn parse_number(&mut self) -> Result<u32> {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                self.pos += 1;
            } else {
                break;
            }
        }
        s.parse()
            .map_err(|_| self.err("expected a number in repeat count"))
    }

    fn parse_string(&mut self) -> Result<Item> {
        self.expect_str("\"")?;
        let mut cps = Vec::new();
        loop {
            match self.peek() {
                None => return Err(self.err("unterminated string literal")),
                Some('"') => {
                    self.pos += 1;
                    break;
                }
                Some('\\') => {
                    self.pos += 1;
                    cps.push(self.parse_escape()?);
                }
                Some(c) => {
                    self.pos += 1;
                    cps.push(c as u32);
                }
            }
        }
        Ok(Item::Literal(cps))
    }

    fn parse_class(&mut self) -> Result<CharSet> {
        self.expect_str("[")?;
        let negated = if self.peek() == Some('^') {
            self.pos += 1;
            true
        } else {
            false
        };
        let mut ranges = Vec::new();
        loop {
            match self.peek() {
                None => return Err(self.err("unterminated char class")),
                Some(']') => {
                    self.pos += 1;
                    break;
                }
                _ => {}
            }
            let lo = self.parse_class_char()?;
            // Range `a-b` (but a trailing `-` before `]` is a literal dash).
            if self.peek() == Some('-') && self.chars.get(self.pos + 1) != Some(&']') {
                self.pos += 1;
                let hi = self.parse_class_char()?;
                ranges.push((lo, hi));
            } else {
                ranges.push((lo, lo));
            }
        }
        Ok(CharSet { negated, ranges })
    }

    fn parse_class_char(&mut self) -> Result<u32> {
        match self.bump() {
            None => Err(self.err("unexpected end of char class")),
            Some('\\') => self.parse_escape(),
            Some(c) => Ok(c as u32),
        }
    }

    fn parse_escape(&mut self) -> Result<u32> {
        match self.bump() {
            Some('n') => Ok('\n' as u32),
            Some('r') => Ok('\r' as u32),
            Some('t') => Ok('\t' as u32),
            Some('\\') => Ok('\\' as u32),
            Some('"') => Ok('"' as u32),
            Some('\'') => Ok('\'' as u32),
            Some(']') => Ok(']' as u32),
            Some('[') => Ok('[' as u32),
            Some('-') => Ok('-' as u32),
            Some('/') => Ok('/' as u32),
            Some('.') => Ok('.' as u32),
            Some('x') => self.parse_hex(2),
            Some('u') => self.parse_hex(4),
            Some('U') => self.parse_hex(8),
            other => Err(self.err(format!("invalid escape \\{other:?}"))),
        }
    }

    fn parse_hex(&mut self, n: usize) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let c = self.bump().ok_or_else(|| self.err("truncated hex escape"))?;
            let d = c
                .to_digit(16)
                .ok_or_else(|| self.err("invalid hex digit"))?;
            v = v * 16 + d;
        }
        Ok(v)
    }
}
