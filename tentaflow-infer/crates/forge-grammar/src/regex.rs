// ===== File: regex.rs — regex (common subset) → grammar AST =====
// Compiles a regular expression to the same grammar AST as GBNF/JSON-schema,
// so the identical automaton enforces it. Supported: literals, `.`
// (any-but-newline), character classes `[...]` with ranges/negation and the
// shorthands `\d \w \s \D \W \S`, groups `(...)`/`(?:...)`, alternation `|`,
// and the quantifiers `* + ?` and `{n}` / `{n,}` / `{n,m}` (an optional
// trailing lazy `?` is accepted and ignored). Anchors `^`/`$` are accepted and
// ignored — a constrained decode always matches the full generated span.
// Backreferences, lookarounds and named groups are not supported.

use forge_types::{ForgeError, Result};

use crate::builder::{AstRule, CharSet, Item};
use crate::schema::SchemaConverter;

/// Convert a whole regex into grammar rules with root rule `root`.
pub fn convert(pattern: &str) -> Result<Vec<AstRule>> {
    let alternates = RegexParser::new(pattern).parse_top()?;
    Ok(vec![AstRule {
        name: "root".into(),
        alternates,
    }])
}

/// Convert a regex into a fresh rule inside an existing converter and return
/// its name (used to embed `string.pattern` inside a JSON string).
pub fn convert_into(conv: &mut SchemaConverter, pattern: &str) -> Result<String> {
    let alternates = RegexParser::new(pattern).parse_top()?;
    let name = conv.reserve_name("re");
    conv.push_rule(AstRule {
        name: name.clone(),
        alternates,
    });
    Ok(name)
}

struct RegexParser {
    chars: Vec<char>,
    pos: usize,
}

impl RegexParser {
    fn new(src: &str) -> Self {
        Self {
            chars: src.chars().collect(),
            pos: 0,
        }
    }

    fn err(&self, msg: impl Into<String>) -> ForgeError {
        ForgeError::Grammar(format!(
            "regex parse error at char {}: {}",
            self.pos,
            msg.into()
        ))
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

    fn parse_top(&mut self) -> Result<Vec<Vec<Item>>> {
        let alts = self.parse_alternation()?;
        if self.pos != self.chars.len() {
            return Err(self.err("unexpected trailing input"));
        }
        Ok(alts)
    }

    fn parse_alternation(&mut self) -> Result<Vec<Vec<Item>>> {
        let mut alts = vec![self.parse_sequence()?];
        while self.peek() == Some('|') {
            self.pos += 1;
            alts.push(self.parse_sequence()?);
        }
        Ok(alts)
    }

    fn parse_sequence(&mut self) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        loop {
            match self.peek() {
                None | Some('|') | Some(')') => break,
                Some('^') | Some('$') => {
                    self.pos += 1; // anchors ignored
                    continue;
                }
                _ => {}
            }
            let atom = self.parse_atom()?;
            let item = self.parse_quantifier(atom)?;
            items.push(item);
        }
        Ok(items)
    }

    fn parse_atom(&mut self) -> Result<Item> {
        match self.peek() {
            Some('(') => {
                self.pos += 1;
                // Non-capturing `(?:` and other `(?` flags: skip to `:` or the
                // flag body; only `(?:` is meaningfully supported.
                if self.peek() == Some('?') {
                    self.pos += 1;
                    if self.peek() == Some(':') {
                        self.pos += 1;
                    } else {
                        return Err(self.err("only non-capturing `(?:` group flags are supported"));
                    }
                }
                let alts = self.parse_alternation()?;
                if self.bump() != Some(')') {
                    return Err(self.err("expected `)`"));
                }
                Ok(Item::Group(alts))
            }
            Some('[') => Ok(Item::Class(self.parse_class()?)),
            Some('.') => {
                self.pos += 1;
                Ok(Item::Class(CharSet::any_but_newline()))
            }
            Some('\\') => {
                self.pos += 1;
                Ok(self.parse_escape_atom()?)
            }
            Some(c) if !")|".contains(c) => {
                self.pos += 1;
                Ok(Item::Literal(vec![c as u32]))
            }
            other => Err(self.err(format!("unexpected {other:?}"))),
        }
    }

    fn parse_quantifier(&mut self, atom: Item) -> Result<Item> {
        let (min, max) = match self.peek() {
            Some('*') => {
                self.pos += 1;
                (0, None)
            }
            Some('+') => {
                self.pos += 1;
                (1, None)
            }
            Some('?') => {
                self.pos += 1;
                (0, Some(1))
            }
            Some('{') => self.parse_brace_count()?,
            _ => return Ok(atom),
        };
        // Optional lazy/possessive marker, ignored (constrained decode has no
        // greediness ambiguity to resolve).
        if matches!(self.peek(), Some('?') | Some('+')) {
            self.pos += 1;
        }
        Ok(Item::Repeat {
            item: Box::new(atom),
            min,
            max,
        })
    }

    fn parse_brace_count(&mut self) -> Result<(u32, Option<u32>)> {
        self.pos += 1; // '{'
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
            return Err(self.err("expected `}`"));
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
        s.parse().map_err(|_| self.err("expected a number"))
    }

    fn parse_class(&mut self) -> Result<CharSet> {
        self.pos += 1; // '['
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
                Some('\\') => {
                    self.pos += 1;
                    // Shorthand inside a class contributes its ranges directly;
                    // a plain escape is a single codepoint that may start a
                    // range.
                    if let Some(mut sh) = self.class_shorthand()? {
                        ranges.append(&mut sh);
                    } else {
                        let c = self.plain_escape()?;
                        self.push_range(&mut ranges, c)?;
                    }
                }
                Some(_) => {
                    let c = self.bump().unwrap() as u32;
                    self.push_range(&mut ranges, c)?;
                }
            }
        }
        Ok(CharSet { negated, ranges })
    }

    /// After reading `lo`, consume an optional `-hi` range tail.
    fn push_range(&mut self, ranges: &mut Vec<(u32, u32)>, lo: u32) -> Result<()> {
        if self.peek() == Some('-') && self.chars.get(self.pos + 1) != Some(&']') {
            self.pos += 1;
            let hi = match self.peek() {
                Some('\\') => {
                    self.pos += 1;
                    self.plain_escape()?
                }
                Some(_) => self.bump().unwrap() as u32,
                None => return Err(self.err("unterminated range")),
            };
            ranges.push((lo, hi));
        } else {
            ranges.push((lo, lo));
        }
        Ok(())
    }

    /// A `\d \w \s` shorthand at the current position (already past `\`),
    /// returning its ranges; `None` for a plain escaped literal (the caller
    /// then reads it via `plain_escape`). Peeks without consuming a literal.
    fn class_shorthand(&mut self) -> Result<Option<Vec<(u32, u32)>>> {
        let c = self.peek().ok_or_else(|| self.err("truncated escape"))?;
        let ranges = match c {
            'd' => vec![('0' as u32, '9' as u32)],
            'w' => vec![
                ('a' as u32, 'z' as u32),
                ('A' as u32, 'Z' as u32),
                ('0' as u32, '9' as u32),
                ('_' as u32, '_' as u32),
            ],
            's' => vec![
                (' ' as u32, ' ' as u32),
                ('\t' as u32, '\t' as u32),
                ('\n' as u32, '\n' as u32),
                ('\r' as u32, '\r' as u32),
                (0x0C, 0x0C),
            ],
            'D' | 'W' | 'S' => {
                return Err(self.err("negated shorthand inside a char class is not supported"))
            }
            _ => return Ok(None),
        };
        self.pos += 1;
        Ok(Some(ranges))
    }

    fn plain_escape(&mut self) -> Result<u32> {
        let c = self.bump().ok_or_else(|| self.err("truncated escape"))?;
        plain_escape_char(c)
    }

    fn parse_escape_atom(&mut self) -> Result<Item> {
        let c = self.peek().ok_or_else(|| self.err("truncated escape"))?;
        let cs = match c {
            'd' => Some(CharSet {
                negated: false,
                ranges: vec![('0' as u32, '9' as u32)],
            }),
            'D' => Some(CharSet {
                negated: true,
                ranges: vec![('0' as u32, '9' as u32)],
            }),
            'w' => Some(CharSet {
                negated: false,
                ranges: vec![
                    ('a' as u32, 'z' as u32),
                    ('A' as u32, 'Z' as u32),
                    ('0' as u32, '9' as u32),
                    ('_' as u32, '_' as u32),
                ],
            }),
            'W' => Some(CharSet {
                negated: true,
                ranges: vec![
                    ('a' as u32, 'z' as u32),
                    ('A' as u32, 'Z' as u32),
                    ('0' as u32, '9' as u32),
                    ('_' as u32, '_' as u32),
                ],
            }),
            's' => Some(CharSet {
                negated: false,
                ranges: vec![
                    (' ' as u32, ' ' as u32),
                    ('\t' as u32, '\t' as u32),
                    ('\n' as u32, '\n' as u32),
                    ('\r' as u32, '\r' as u32),
                    (0x0C, 0x0C),
                ],
            }),
            'S' => Some(CharSet {
                negated: true,
                ranges: vec![
                    (' ' as u32, ' ' as u32),
                    ('\t' as u32, '\t' as u32),
                    ('\n' as u32, '\n' as u32),
                    ('\r' as u32, '\r' as u32),
                    (0x0C, 0x0C),
                ],
            }),
            _ => None,
        };
        if let Some(cs) = cs {
            self.pos += 1;
            return Ok(Item::Class(cs));
        }
        let cp = self.plain_escape()?;
        Ok(Item::Literal(vec![cp]))
    }
}

/// A single-char escape's codepoint value.
fn plain_escape_char(c: char) -> Result<u32> {
    Ok(match c {
        'n' => '\n' as u32,
        'r' => '\r' as u32,
        't' => '\t' as u32,
        'f' => 0x0C,
        '0' => 0,
        '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '^' | '$'
        | '-' | '/' => c as u32,
        _ => c as u32,
    })
}
