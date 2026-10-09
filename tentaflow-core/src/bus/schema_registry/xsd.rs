// =============================================================================
// File: bus/schema_registry/xsd.rs — XSD subset validator (F4 B4)
// =============================================================================
// SUM/tentabus/PLAN-F4-REST.md §B.4 and DECYZJE-2026-09-22 (XSD row): no
// maintained pure-Rust XSD validator exists and libxml2 would be a native
// dependency, so this is a HAND-WRITTEN SUBSET on `quick-xml` (already a
// dependency) and `regex` (already a dependency). As with `json_schema`,
// anything outside the subset is REJECTED at `compile` (registration) time
// with a message naming the construct — never silently ignored.
//
// Supported subset
//   - One schema document. Top level: `element` (the permitted document
//     roots, at least one), named `complexType` / `simpleType`, `annotation`.
//   - `element` with `name`, `type` or an inline type, `minOccurs`,
//     `maxOccurs` (non-negative integer or `unbounded`, capped at 100000).
//   - `complexType` with ONE of `sequence` / `choice` / `all` (nested
//     `sequence`/`choice` allowed, each with its own occurrence bounds;
//     `all` only as the top group, children occur at most once), or
//     `simpleContent/extension` (text with attributes), or empty content,
//     followed by `attribute` (`use` = required | optional).
//   - Simple types `xs:string`, `xs:int`, `xs:integer`, `xs:decimal`,
//     `xs:boolean`, `xs:date`, `xs:dateTime` and `simpleType/restriction`
//     with `minLength`, `maxLength` (string only), `pattern`, `enumeration`.
//     Several `pattern`s (or `enumeration`s) in ONE restriction are
//     alternatives; restrictions stacked through named bases all apply.
//   - Rejected with a clear error: `import`/`include`/`redefine`/`override`,
//     `group`/`attributeGroup`, `any`/`anyAttribute`, `key`/`keyref`/`unique`,
//     `complexContent`, `list`/`union`, `mixed="true"`, `element ref=`,
//     `default`/`fixed`/`nillable`/`abstract`/`substitutionGroup`, other
//     facets, other built-in types, DTDs, namespace declarations anywhere but
//     on `xs:schema`, and an element without a type (that is `xs:anyType`).
//     Two child elements of one content model with the same name are
//     rejected too (they would need type disambiguation by position).
//
// Namespaces: a schema may carry ONE `targetNamespace`; types are referenced
// through the prefixes declared on `xs:schema`. Instance documents are NOT
// namespace-checked: elements are matched by LOCAL name (the prefix is
// dropped), attributes by their literal name, `xmlns`/`xmlns:*` and
// `xsi:schemaLocation`/`xsi:noNamespaceSchemaLocation` are ignored, other
// `xsi:` attributes (`xsi:type`, `xsi:nil`) are violations. This mirrors
// `payload_format::xml`, which also never resolves namespaces. `elementForm*`
// is accepted and has no effect for the same reason.
//
// Values: every built-in except `xs:string` is whitespace-collapsed before
// checking. `xs:decimal` has no exponent form; `xs:date`/`xs:dateTime` accept
// an optional `Z` / `±hh:mm` zone, years of at least four digits and reject
// `24:00:00` and leap seconds. `enumeration` compares the CANONICAL value for
// numbers and booleans (`01` equals `1`, `1` equals `true`), the literal text
// for everything else.
//
// `pattern` is translated from the XSD regex dialect to the Rust regex syntax
// (`regex-automata`) and wrapped in `\A(?:…)\z` (XSD patterns are implicitly anchored, and
// `^`/`$` are ordinary characters in XSD, so they are escaped). Differences
// that remain: `.` excludes `\n` and `\r` (as in XSD); `\d` is `\p{Nd}` and
// `\s` is `[ \t\n\r]` (as in XSD); constructs that mean something different
// or do not exist in XSD (`\i \I \c \C \w \W`, character-class subtraction,
// `\p{IsBlock}`, `(?…)` groups, lazy quantifiers, `\b`) are rejected at
// compile time. Every group is non-capturing (XSD has no captures or
// backreferences), and matching is linear time: a lazy DFA (`hybrid`) scans a
// value at one budget unit per byte and, when its cache thrashes and it gives
// up, a Pike VM takes over at `states x length` units charged first, so a
// hostile pattern cannot cause catastrophic backtracking or unmetered work.
//
// Resource bounds are structural. One meter (`Budget`) is charged by every
// expensive primitive BEFORE it runs — `SimpleType::check` (value length per
// pattern, enumeration lookup), enumeration intersection, NFA
// closure/step, attribute processing, compile work — and `check` cannot be
// called without one. Validation (`MAX_VALIDATION_STEPS`), comparison
// (`MAX_COMPAT_WORK`) and compilation (`MAX_COMPILE_WORK`) each own a budget;
// running out is `LimitExceeded`, never a verdict. On top of that, static caps
// refuse a schema at compile time: enumeration value and total bytes,
// pattern length, count and compiled size, a per-schema memory proxy
// (`pattern_memory`), and attributes per type.
//
// Content models compile to an NFA (Thompson construction; counted
// occurrences are expanded, bounded by a state budget) and are simulated with
// state sets, so validation is linear in children x active states with a hard
// step budget (a document that exceeds it, or nests deeper than the limit, is
// `LimitExceeded`: the check gave up, the document is not known invalid). Violation
// messages are PATH + CONSTRAINT only: never element text, attribute values
// or a pattern's text — they reach audit rows, warn logs and DLQ headers.
//
// `derive_subschema` works on the parsed declarations: it removes the
// declarations of the root's DIRECT CHILD elements that the field policy does
// not allow (the same addresses `payload_format::xml` projects: literal child
// names, a `p:name` policy entry also matches the declared `name`), keeps the
// root's attributes, makes a `choice` optional when a whole alternative
// disappears, and drops declarations that become unreachable. The result is
// itself an XSD in the supported subset (namespace-free, `xs:` prefix) and is
// deterministic for the same inputs.
//
// `check_compatibility` decides language INCLUSION conservatively on the
// compiled schemas: backward = every document valid under the old schema is
// valid under the new one, forward = the reverse, full = both. Roots, element
// content models (exact NFA inclusion by product construction, child types
// compared recursively), attributes and simple types (built-in widening
// int < integer < decimal < string, length bounds, enumerations, identical
// patterns) are compared; whenever inclusion cannot be PROVEN the change is
// refused, never waved through.
// =============================================================================

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::{Decoder, Reader, XmlVersion};
use regex_automata::hybrid::dfa::{
    Builder as DfaBuilder, Cache as DfaCache, Config as DfaConfig, DFA,
};
use regex_automata::nfa::thompson::pikevm::PikeVM;
use regex_automata::nfa::thompson::{self, WhichCaptures};
use regex_automata::util::pool::Pool;
use regex_automata::util::syntax;
use regex_automata::{Anchored, Input};

use super::{
    Compatibility, CompiledSchema, SchemaError, SchemaKindOps, ValidationBudget,
    MAX_SCHEMA_TEXT_BYTES,
};

const XSD_NS: &str = "http://www.w3.org/2001/XMLSchema";
const XSI_PREFIX: &str = "xsi:";
const MAX_SCHEMA_DEPTH: usize = 32;
const MAX_SCHEMA_NODES: usize = 20_000;
const MAX_OCCURS: u32 = 100_000;
const MAX_NFA_STATES: usize = 10_000;
const MAX_TOTAL_NFA_STATES: usize = 50_000;
const MAX_PATTERNS: usize = 256;
/// A real-world pattern (identifier, date, code list shape) is a few dozen
/// characters; 512 leaves room for long alternations without letting one
/// facet carry a whole data set.
const MAX_PATTERN_CHARS: usize = 512;
const MAX_ENUMERATION_VALUES: usize = 4096;
/// Code values in healthcare and finance schemas (ICD, LOINC, ISO currency,
/// country, status) are well under 100 bytes; 1 KiB is generous.
const MAX_ENUM_VALUE_BYTES: usize = 1024;
/// Half of the schema text limit: 4096 values at 32 bytes fit with room to
/// spare, while a schema cannot spend its whole text on one enumeration.
const MAX_ENUM_TOTAL_BYTES: usize = 128 * 1024;
/// Declared attributes of one complex type; the widest real types (CDA,
/// FHIR-like records) carry a few dozen.
const MAX_ATTRIBUTES_PER_TYPE: usize = 1024;
const MAX_NAME_CHARS: usize = 128;
/// Size limit of one pattern's compiled program. Unicode makes the range wide:
/// `\d` is `\p{Nd}` and costs ~7 KiB, so a PESEL (`\d{11}`) needs ~64 KiB and a
/// 26-digit IBAN body ~192 KiB; this admits those while `\p{L}{60}` (4 MiB) is
/// refused outright.
const PATTERN_NFA_LIMIT: usize = 256 * 1024;
/// Lazy-DFA cache of a pattern: twice the smallest capacity its automaton
/// builds with (1 KiB for `q1x`, 32 KiB for an IBAN), at least
/// `PATTERN_CACHE_FLOOR` and refused above `PATTERN_CACHE_MAX`.
const PATTERN_CACHE_FLOOR: usize = 16 * 1024;
const PATTERN_CACHE_MAX: usize = 1024 * 1024;
/// Lazy-DFA caches a pattern is budgeted for: one per concurrent validator.
/// The pool hands every concurrently running match its own cache and keeps
/// it, so a pattern holds as many caches as it has seen simultaneous matches;
/// `pattern_memory` accounts for this many, each further concurrent match
/// adds at most `cache_capacity + fresh_cache_bytes`.
const POOLED_CACHES: usize = 4;
/// Memory proxy per schema, charged per pattern as
/// `nfa.memory_usage() + POOLED_CACHES x (cache_capacity + fresh_cache_bytes)`:
/// the forward program (shared with the Pike VM fallback; no reverse program
/// is built, matching is anchored and forward-only) plus the lazy-DFA caches
/// of up to `POOLED_CACHES` concurrent matches (`fresh_cache_bytes` is the
/// NFA-sized sparse sets and start table a cache holds before any state is
/// built). A Pike VM cache (~64 bytes per NFA state) exists only during a
/// charged fallback and is dropped afterwards, so it is not retained memory.
const MAX_PATTERN_MEMORY: usize = 32 * 1024 * 1024;
const PATTERN_NEST_LIMIT: u32 = 32;
const MAX_DOC_DEPTH: usize = 128;
/// One unit is roughly 10 ns of work; 50M units bound a document to about
/// half a second in the worst case.
const MAX_VALIDATION_STEPS: u64 = 50_000_000;
const COST_EVENT: u64 = 8;
const COST_ELEMENT: u64 = 16;
const COST_ATTRIBUTE: u64 = 16;
const MAX_COMPAT_PAIRS: usize = 50_000;
const MAX_COMPAT_WORK: u64 = 20_000_000;
/// Work of compiling one schema: declarations, attributes, pattern and
/// enumeration bytes, NFA states. A schema is at most 256 KiB, so this is
/// far above anything legitimate and only stops a shape that multiplies.
const MAX_COMPILE_WORK: u64 = 5_000_000;
const COMPILE_WORK_REFUSAL: &str = "the schema exceeds the compile work limit; simplify it";

const SUPPORTED_BUILTINS: &str = "string, int, integer, decimal, boolean, date, dateTime";

/// The single meter every expensive primitive charges. An absurd charge
/// (value length x pattern count) cannot wrap around: it simply does not fit.
#[derive(Debug)]
struct Budget {
    used: u64,
    limit: u64,
}

/// A [`Budget`] ran out: the check stopped before it could decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LimitExceeded;

impl Budget {
    fn new(limit: u64) -> Budget {
        Budget { used: 0, limit }
    }

    /// Check before commit: a charge that does not fit is refused WITHOUT
    /// being added, so `used` never exceeds `limit` and a shared batch is
    /// debited only for work that was actually allowed to run.
    fn charge(&mut self, units: u64) -> Result<(), LimitExceeded> {
        match self.used.checked_add(units) {
            Some(total) if total <= self.limit => {
                self.used = total;
                Ok(())
            }
            _ => Err(LimitExceeded),
        }
    }
}

fn invalid(msg: impl Into<String>) -> SchemaError {
    SchemaError::Invalid(msg.into())
}

// =============================================================================
// Built-in simple types and lexical checks
// =============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Builtin {
    String,
    Boolean,
    Decimal,
    Integer,
    Int,
    Date,
    DateTime,
}

impl Builtin {
    fn from_local(local: &str) -> Option<Builtin> {
        Some(match local {
            "string" => Builtin::String,
            "boolean" => Builtin::Boolean,
            "decimal" => Builtin::Decimal,
            "integer" => Builtin::Integer,
            "int" => Builtin::Int,
            "date" => Builtin::Date,
            "dateTime" => Builtin::DateTime,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Builtin::String => "string",
            Builtin::Boolean => "boolean",
            Builtin::Decimal => "decimal",
            Builtin::Integer => "integer",
            Builtin::Int => "int",
            Builtin::Date => "date",
            Builtin::DateTime => "dateTime",
        }
    }

    fn is_string(self) -> bool {
        self == Builtin::String
    }

    /// Whether every lexical value of `self` is also a valid lexical value of
    /// `target` (the order used for compatibility: int < integer < decimal,
    /// everything < string).
    fn widens_to(self, target: Builtin) -> bool {
        self == target
            || target == Builtin::String
            || matches!(
                (self, target),
                (Builtin::Int, Builtin::Integer)
                    | (Builtin::Int, Builtin::Decimal)
                    | (Builtin::Integer, Builtin::Decimal)
            )
    }
}

fn is_xml_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn valid_integer(s: &str) -> bool {
    all_digits(s.strip_prefix(['+', '-']).unwrap_or(s))
}

fn valid_decimal(s: &str) -> bool {
    let s = s.strip_prefix(['+', '-']).unwrap_or(s);
    match s.split_once('.') {
        None => all_digits(s),
        Some((int, frac)) => {
            (int.is_empty() || all_digits(int))
                && (frac.is_empty() || all_digits(frac))
                && !(int.is_empty() && frac.is_empty())
        }
    }
}

fn valid_int(s: &str) -> bool {
    if !valid_integer(s) {
        return false;
    }
    let negative = s.starts_with('-');
    let digits = s.trim_start_matches(['+', '-']).trim_start_matches('0');
    if digits.len() > 10 {
        return false;
    }
    let magnitude: i64 = if digits.is_empty() {
        0
    } else {
        digits.parse().unwrap_or(i64::MAX)
    };
    if negative {
        magnitude <= 2_147_483_648
    } else {
        magnitude <= 2_147_483_647
    }
}

/// Length of a valid `YYYY-MM-DD` prefix of `s` (year of at least four
/// digits), or `None`.
fn date_prefix_len(s: &str) -> Option<usize> {
    let year_len = s.bytes().take_while(u8::is_ascii_digit).count();
    if !(4..=6).contains(&year_len) {
        return None;
    }
    let rest = s.get(year_len..)?;
    let bytes = rest.as_bytes();
    if bytes.len() < 6 || bytes[0] != b'-' || bytes[3] != b'-' {
        return None;
    }
    let month = rest.get(1..3)?;
    let day = rest.get(4..6)?;
    if !(all_digits(month) && all_digits(day)) {
        return None;
    }
    let year: i32 = s[..year_len].parse().ok()?;
    // XSD 1.0 has no year 0000.
    if year == 0 {
        return None;
    }
    chrono::NaiveDate::from_ymd_opt(year, month.parse().ok()?, day.parse().ok()?)?;
    Some(year_len + 6)
}

fn valid_timezone(s: &str) -> bool {
    if s.is_empty() || s == "Z" {
        return true;
    }
    let Some(zone) = s.strip_prefix(['+', '-']) else {
        return false;
    };
    let Some((h, m)) = zone.split_once(':') else {
        return false;
    };
    if h.len() != 2 || m.len() != 2 || !all_digits(h) || !all_digits(m) {
        return false;
    }
    let (h, m): (u32, u32) = (h.parse().unwrap_or(99), m.parse().unwrap_or(99));
    m <= 59 && (h < 14 || (h == 14 && m == 0))
}

fn valid_date(s: &str) -> bool {
    date_prefix_len(s).is_some_and(|n| valid_timezone(&s[n..]))
}

fn valid_date_time(s: &str) -> bool {
    let Some(n) = date_prefix_len(s) else {
        return false;
    };
    let Some(time) = s[n..].strip_prefix('T') else {
        return false;
    };
    let Some(clock) = time.get(..8) else {
        return false;
    };
    let b = clock.as_bytes();
    if b[2] != b':' || b[5] != b':' {
        return false;
    }
    let part = |range: std::ops::Range<usize>| {
        let p = &clock[range];
        if all_digits(p) {
            p.parse::<u32>().ok()
        } else {
            None
        }
    };
    let (Some(h), Some(m), Some(sec)) = (part(0..2), part(3..5), part(6..8)) else {
        return false;
    };
    if h > 23 || m > 59 || sec > 59 {
        return false;
    }
    let mut rest = &time[8..];
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return false;
        }
        rest = &frac[digits..];
    }
    valid_timezone(rest)
}

/// `value` is already trimmed for every built-in except `xs:string`.
fn valid_lexical(builtin: Builtin, value: &str) -> bool {
    match builtin {
        Builtin::String => true,
        Builtin::Boolean => matches!(value, "true" | "false" | "1" | "0"),
        Builtin::Decimal => valid_decimal(value),
        Builtin::Integer => valid_integer(value),
        Builtin::Int => valid_int(value),
        Builtin::Date => valid_date(value),
        Builtin::DateTime => valid_date_time(value),
    }
}

/// Value-space form used to compare enumeration members: `01` and `+1` are
/// the same integer, `1` and `true` the same boolean.
fn canonical(builtin: Builtin, value: &str) -> String {
    match builtin {
        Builtin::Boolean => match value {
            "1" | "true" => "true".to_string(),
            _ => "false".to_string(),
        },
        Builtin::Integer | Builtin::Int => {
            let negative = value.starts_with('-');
            let digits = value.trim_start_matches(['+', '-']).trim_start_matches('0');
            if digits.is_empty() {
                "0".to_string()
            } else if negative {
                format!("-{digits}")
            } else {
                digits.to_string()
            }
        }
        Builtin::Decimal => {
            let negative = value.starts_with('-');
            let body = value.trim_start_matches(['+', '-']);
            let (int, frac) = body.split_once('.').unwrap_or((body, ""));
            let int = int.trim_start_matches('0');
            let frac = frac.trim_end_matches('0');
            let int = if int.is_empty() { "0" } else { int };
            let mut out = String::new();
            if negative && !(int == "0" && frac.is_empty()) {
                out.push('-');
            }
            out.push_str(int);
            if !frac.is_empty() {
                out.push('.');
                out.push_str(frac);
            }
            out
        }
        Builtin::String | Builtin::Date | Builtin::DateTime => value.to_string(),
    }
}

// =============================================================================
// XSD regex dialect -> Rust regex
// =============================================================================

/// Translates an XSD `pattern` to an anchored Rust regex source. See the
/// module header for the list of deliberate differences.
fn translate_pattern(pattern: &str) -> Result<String, String> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::from("\\A(?:");
    let mut i = 0;
    let mut in_class = false;
    // Items seen in the current class, and whether the last one expands to
    // several characters (`\s`, `\d`, `\p{..}`), which cannot bound a range.
    let mut class_items = 0usize;
    let mut prev_multi = false;
    let mut range_open = false;
    let mut prev_quantifier = false;
    // Without this, `a)|(b` would close the anchoring group opened above and
    // leave the two halves of the alternation unanchored.
    let mut depth = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\\' => {
                let Some(&n) = chars.get(i + 1) else {
                    return Err("pattern ends with a lone backslash".to_string());
                };
                i += 2;
                let multi = matches!(n, 'd' | 'D' | 's' | 'S' | 'p' | 'P');
                if in_class {
                    if multi && range_open {
                        return Err(format!("\\{n} cannot be the end of a character range"));
                    }
                    class_items += 1;
                    prev_multi = multi;
                    range_open = false;
                }
                match n {
                    'n' | 'r' | 't' | '\\' | '|' | '.' | '-' | '^' | '?' | '*' | '+' | '{'
                    | '}' | '(' | ')' | '[' | ']' => {
                        out.push('\\');
                        out.push(n);
                    }
                    'd' => out.push_str("\\p{Nd}"),
                    'D' => out.push_str("\\P{Nd}"),
                    's' => out.push_str(if in_class {
                        " \\t\\n\\r"
                    } else {
                        "[ \\t\\n\\r]"
                    }),
                    'S' => {
                        if in_class {
                            return Err("\\S inside a character class is not supported".into());
                        }
                        out.push_str("[^ \\t\\n\\r]");
                    }
                    'p' | 'P' => {
                        if chars.get(i) != Some(&'{') {
                            return Err("\\p must be followed by {Category}".to_string());
                        }
                        let close = chars[i..]
                            .iter()
                            .position(|&c| c == '}')
                            .ok_or("unterminated \\p{...}")?;
                        let name: String = chars[i + 1..i + close].iter().collect();
                        if name.is_empty()
                            || name.len() > 2
                            || !name.chars().all(|c| c.is_ascii_alphabetic())
                        {
                            return Err(format!(
                                "\\{n}{{{name}}} is not supported (only general categories like \
                                 \\p{{L}} or \\p{{Lu}}, no Unicode blocks)"
                            ));
                        }
                        out.push('\\');
                        out.push(n);
                        out.push('{');
                        out.push_str(&name);
                        out.push('}');
                        i += close + 1;
                    }
                    other => {
                        return Err(format!(
                            "escape \\{other} is not supported in an XSD pattern"
                        ));
                    }
                }
                prev_quantifier = false;
                continue;
            }
            '[' if in_class => {
                return Err("'[' inside a character class must be escaped".to_string());
            }
            '[' => {
                in_class = true;
                class_items = 0;
                prev_multi = false;
                range_open = false;
                out.push('[');
                if chars.get(i + 1) == Some(&'^') {
                    out.push('^');
                    i += 1;
                }
                // The regex crate reads a leading ']' as a literal while this
                // translator would close the class there, so the two would
                // disagree on where the class ends.
                if chars.get(i + 1) == Some(&']') {
                    return Err("']' inside a character class must be escaped".to_string());
                }
                prev_quantifier = false;
            }
            ']' if in_class => {
                in_class = false;
                out.push(']');
            }
            '-' if in_class && chars.get(i + 1) == Some(&'[') => {
                return Err("character class subtraction is not supported".to_string());
            }
            '-' if in_class && chars.get(i + 1) == Some(&'-') => {
                return Err("'--' inside a character class is not supported".to_string());
            }
            '-' if in_class => {
                let literal = class_items == 0 || chars.get(i + 1) == Some(&']');
                if prev_multi && !literal {
                    return Err(
                        "a multi-character escape cannot be the start of a character range"
                            .to_string(),
                    );
                }
                out.push('-');
                range_open = !literal;
                prev_multi = false;
                class_items += 1;
            }
            '&' | '~' if in_class => {
                out.push('\\');
                out.push(c);
                class_items += 1;
                prev_multi = false;
                range_open = false;
            }
            '^' | '$' if !in_class => {
                out.push('\\');
                out.push(c);
                prev_quantifier = false;
            }
            '.' if !in_class => {
                out.push_str("[^\\n\\r]");
                prev_quantifier = false;
            }
            '(' if !in_class => {
                if chars.get(i + 1) == Some(&'?') {
                    return Err("'(?' groups are not part of the XSD regex dialect".to_string());
                }
                out.push_str("(?:");
                depth += 1;
                prev_quantifier = false;
            }
            ')' if !in_class => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| "unbalanced ')' in the pattern".to_string())?;
                out.push(')');
                prev_quantifier = false;
            }
            '?' | '*' | '+' if !in_class => {
                if prev_quantifier {
                    return Err("a quantifier cannot follow another quantifier".to_string());
                }
                out.push(c);
                prev_quantifier = true;
            }
            '}' if !in_class => {
                out.push('}');
                prev_quantifier = true;
            }
            _ => {
                out.push(c);
                if in_class {
                    class_items += 1;
                    prev_multi = false;
                    range_open = false;
                } else {
                    prev_quantifier = false;
                }
            }
        }
        i += 1;
    }
    if in_class {
        return Err("unterminated character class".to_string());
    }
    if depth != 0 {
        return Err("unterminated group in the pattern".to_string());
    }
    out.push_str(")\\z");
    Ok(out)
}

/// One compiled pattern: an anchored full-match test over a string.
#[derive(Debug)]
struct Matcher {
    dfa: DFA,
    pike: PikeVM,
    cache_capacity: usize,
    /// Pike VM work per input byte: NFA states plus every sparse transition
    /// range and union alternate a step may have to scan.
    width: u64,
    /// One lazy-DFA cache per concurrently running match, reused across
    /// matches (a fresh cache zeroes NFA-sized sets and recomputes every
    /// transition, so it is charged when built).
    caches: DfaCachePool,
    memory: usize,
}

type DfaCachePool = Pool<MatchCache, Box<dyn Fn() -> MatchCache + Send + Sync>>;

#[derive(Debug)]
struct MatchCache {
    cache: DfaCache,
    /// Not yet charged for being built.
    fresh: bool,
}

/// Work of one Pike VM step over the whole automaton, counting what a
/// Unicode class costs: one scan per sparse range, per union alternate.
fn automaton_width(nfa: &thompson::NFA) -> u64 {
    nfa.states().iter().fold(0u64, |width, state| {
        let ranges = match state {
            thompson::State::Sparse(t) => t.transitions.len(),
            thompson::State::Dense(_) => 256,
            thompson::State::Union { alternates } => alternates.len(),
            thompson::State::BinaryUnion { .. } => 2,
            _ => 0,
        };
        width.saturating_add(1).saturating_add(ranges as u64)
    })
}

/// The reason of a pattern `BuildError`, in a stable phrase the dashboard
/// maps: regex-syntax words its parse errors as a header, the pattern and a
/// final `error: <reason>` line; only that reason is kept, never the text.
fn build_error_reason(e: &thompson::BuildError) -> String {
    let source = std::error::Error::source(e).map(ToString::to_string);
    let reason = source
        .as_deref()
        .and_then(|s| {
            s.lines()
                .rev()
                .find_map(|l| l.trim().strip_prefix("error: "))
        })
        .unwrap_or("the pattern could not be built");
    reason.to_string()
}

/// A pattern longer than `MAX_PATTERN_CHARS`, malformed, or too large to
/// compile is an error naming the pattern's problem, not its text.
fn build_pattern(source: &str) -> Result<Matcher, String> {
    if source.chars().count() > MAX_PATTERN_CHARS {
        return Err(format!("pattern exceeds {MAX_PATTERN_CHARS} characters"));
    }
    let translated = translate_pattern(source)?;
    let too_large = || {
        "pattern is not a valid or supported regular expression: the compiled pattern is too \
         large"
            .to_string()
    };
    let nfa = thompson::Compiler::new()
        .configure(
            thompson::Config::new()
                .nfa_size_limit(Some(PATTERN_NFA_LIMIT))
                .which_captures(WhichCaptures::Implicit),
        )
        .syntax(syntax::Config::new().nest_limit(PATTERN_NEST_LIMIT))
        .build(&translated)
        .map_err(|e| {
            if e.size_limit().is_some() {
                too_large()
            } else {
                format!(
                    "pattern is not a valid or supported regular expression: {}",
                    build_error_reason(&e)
                )
            }
        })?;
    let build_dfa = |cache_capacity: usize| {
        // Building a state costs about one step per NFA state, so the lazy
        // DFA may clear its cache only while it has searched enough bytes
        // per cached state to keep that cost below the byte's own charge.
        let config = DfaConfig::new()
            .cache_capacity(cache_capacity)
            .minimum_cache_clear_count(Some(0))
            .minimum_bytes_per_state(Some((nfa.states().len() / 2).max(10)));
        DfaBuilder::new()
            .configure(config)
            .build_from_nfa(nfa.clone())
            .ok()
    };
    let mut smallest = 1024;
    while build_dfa(smallest).is_none() {
        smallest *= 2;
        if smallest * 2 > PATTERN_CACHE_MAX {
            return Err(too_large());
        }
    }
    let cache_capacity = (2 * smallest).max(PATTERN_CACHE_FLOOR);
    let dfa = build_dfa(cache_capacity).ok_or_else(too_large)?;
    let pike = PikeVM::new_from_nfa(nfa.clone())
        .map_err(|e| format!("pattern is not a valid or supported regular expression: {e}"))?;
    let fresh_cache_bytes = dfa.create_cache().memory_usage();
    let pool_dfa = dfa.clone();
    Ok(Matcher {
        memory: nfa
            .memory_usage()
            .saturating_add(POOLED_CACHES.saturating_mul(cache_capacity + fresh_cache_bytes)),
        width: automaton_width(&nfa),
        dfa,
        pike,
        cache_capacity,
        caches: Pool::new(Box::new(move || MatchCache {
            cache: pool_dfa.create_cache(),
            fresh: true,
        })),
    })
}

impl Matcher {
    /// Resident bytes this pattern may hold; see `MAX_PATTERN_MEMORY`.
    fn memory_bytes(&self) -> usize {
        self.memory
    }

    /// Whether the whole of `value` matches. Costs one unit per byte of the
    /// lazy-DFA scan, plus the bytes of automaton state the scan had to
    /// build (a new state costs about its encoded NFA set) and, once per
    /// cache, `states` units for building it. When the lazy DFA gives up (its
    /// cache thrashed on a pattern whose state space explodes), the cache it
    /// burned and the Pike VM's `width x length` are charged BEFORE the Pike
    /// VM runs.
    fn is_match(&self, value: &str, budget: &mut Budget) -> Result<bool, LimitExceeded> {
        let len = value.len() as u64;
        budget.charge(1 + len)?;
        let input = Input::new(value).anchored(Anchored::Yes).earliest(true);
        let mut guard = self.caches.get();
        if guard.fresh {
            budget.charge(self.dfa.get_nfa().states().len() as u64)?;
            guard.fresh = false;
        }
        let before = guard.cache.memory_usage();
        match self.dfa.try_search_fwd(&mut guard.cache, &input) {
            Ok(found) => {
                let grown = guard.cache.memory_usage().saturating_sub(before);
                budget.charge(grown as u64)?;
                Ok(found.is_some())
            }
            Err(_) => {
                guard.cache.reset(&self.dfa);
                drop(guard);
                budget.charge(
                    (self.cache_capacity as u64).saturating_add(len.saturating_mul(self.width)),
                )?;
                let mut cache = self.pike.create_cache();
                Ok(self.pike.is_match(&mut cache, input))
            }
        }
    }
}

// =============================================================================
// Declarations (parsed form; also the source of `derive_subschema`)
// =============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Max {
    Bounded(u32),
    Unbounded,
}

#[derive(Clone, Debug, PartialEq)]
enum SimpleBase {
    Builtin(Builtin),
    Named(String),
}

#[derive(Clone, Debug, PartialEq)]
enum TypeUse {
    Builtin(Builtin),
    Named(String),
    Simple(Box<SimpleDef>),
    Complex(Box<ComplexDef>),
}

#[derive(Clone, Debug, Default, PartialEq)]
struct FacetDef {
    min_length: Option<u32>,
    max_length: Option<u32>,
    patterns: Vec<String>,
    enumeration: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct SimpleDef {
    base: SimpleBase,
    facets: FacetDef,
}

#[derive(Clone, Debug, PartialEq)]
struct AttrDef {
    name: String,
    ty: TypeUse,
    required: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupKind {
    Sequence,
    Choice,
    All,
}

#[derive(Clone, Debug, PartialEq)]
struct ElementDef {
    name: String,
    ty: TypeUse,
    min: u32,
    max: Max,
}

#[derive(Clone, Debug, PartialEq)]
enum Item {
    Element(ElementDef),
    Group(GroupDef),
}

#[derive(Clone, Debug, PartialEq)]
struct GroupDef {
    kind: GroupKind,
    min: u32,
    max: Max,
    items: Vec<Item>,
    /// `xs:all` elements declared with `maxOccurs="0"`: they never match,
    /// but their type and patterns are still checked at compile time.
    prohibited: Vec<ElementDef>,
}

#[derive(Clone, Debug, PartialEq)]
enum ComplexContent {
    Empty,
    Text(SimpleBase),
    Group(GroupDef),
}

#[derive(Clone, Debug, PartialEq)]
struct ComplexDef {
    content: ComplexContent,
    attributes: Vec<AttrDef>,
}

#[derive(Clone, Debug, PartialEq)]
struct Doc {
    /// The source declared a `targetNamespace`, so instances are likely to
    /// spell element names with a prefix.
    namespaced: bool,
    description: Option<String>,
    elements: Vec<ElementDef>,
    simple_types: Vec<(String, SimpleDef)>,
    complex_types: Vec<(String, ComplexDef)>,
}

// =============================================================================
// Schema text -> element tree
// =============================================================================

struct XNode {
    local: String,
    xsd: bool,
    attrs: Vec<(String, String)>,
    children: Vec<XNode>,
    text: String,
}

impl XNode {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn check_attrs(&self, allowed: &[&str]) -> Result<(), SchemaError> {
        for (key, _) in &self.attrs {
            if key == "id" || allowed.contains(&key.as_str()) {
                continue;
            }
            if key == "ref" {
                return Err(invalid(format!(
                    "xs:{} ref= is not supported; declare it in place",
                    self.local
                )));
            }
            return Err(invalid(format!(
                "attribute '{key}' on xs:{} is not supported in this XSD subset",
                self.local
            )));
        }
        Ok(())
    }
}

struct Namespaces {
    default: Option<String>,
    prefixes: HashMap<String, String>,
}

impl Namespaces {
    fn resolve(&self, prefix: Option<&str>) -> Option<&str> {
        match prefix {
            Some(p) => self.prefixes.get(p).map(String::as_str),
            None => self.default.as_deref(),
        }
    }
}

fn utf8<'a>(bytes: &'a [u8], what: &str) -> Result<&'a str, SchemaError> {
    std::str::from_utf8(bytes).map_err(|_| invalid(format!("{what} is not valid UTF-8")))
}

fn append_ref(r: &BytesRef, out: &mut String) -> Result<(), String> {
    match r.resolve_char_ref() {
        Ok(Some(c)) => {
            out.push(c);
            return Ok(());
        }
        Ok(None) => {}
        Err(_) => return Err("invalid character reference".to_string()),
    }
    // Fixed wording: this also runs on instance documents, whose reference
    // names must not reach audit rows and DLQ headers.
    let name = r
        .decode()
        .map_err(|_| "entity reference is not valid text".to_string())?;
    match resolve_predefined_entity(&name) {
        Some(s) => {
            out.push_str(s);
            Ok(())
        }
        None => Err(
            "entity references other than the five predefined ones are not supported".to_string(),
        ),
    }
}

fn build_tree(text: &str) -> Result<(XNode, Namespaces), SchemaError> {
    let mut reader = Reader::from_str(text);
    let mut stack: Vec<XNode> = Vec::new();
    let mut root: Option<XNode> = None;
    let mut namespaces: Option<Namespaces> = None;
    let mut annotation_depth: Option<usize> = None;
    let mut nodes = 0usize;

    let attach = |stack: &mut Vec<XNode>,
                  root: &mut Option<XNode>,
                  node: XNode|
     -> Result<(), SchemaError> {
        if let Some(parent) = stack.last_mut() {
            parent.children.push(node);
        } else if root.is_some() {
            return Err(invalid("more than one root element"));
        } else {
            *root = Some(node);
        }
        Ok(())
    };

    loop {
        let event = reader
            .read_event()
            .map_err(|e| invalid(format!("not well-formed XML: {e}")))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                nodes += 1;
                if nodes > MAX_SCHEMA_NODES {
                    return Err(invalid(format!(
                        "schema has more than {MAX_SCHEMA_NODES} elements"
                    )));
                }
                if stack.len() + 1 > MAX_SCHEMA_DEPTH {
                    return Err(invalid(format!(
                        "schema is nested deeper than {MAX_SCHEMA_DEPTH} levels"
                    )));
                }
                let qname = utf8(e.name().as_ref(), "element name")?.to_string();
                let mut attrs = Vec::new();
                let mut declared = Namespaces {
                    default: None,
                    prefixes: HashMap::new(),
                };
                for a in e.attributes() {
                    let a = a.map_err(|e| invalid(format!("malformed attribute: {e}")))?;
                    let key = utf8(a.key.as_ref(), "attribute name")?.to_string();
                    let value = a
                        .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                        .map_err(|e| invalid(format!("malformed attribute value: {e}")))?
                        .into_owned();
                    if key == "xmlns" || key.starts_with("xmlns:") {
                        if !stack.is_empty() && annotation_depth.is_none() {
                            return Err(invalid(
                                "namespace declarations are only supported on xs:schema",
                            ));
                        }
                        if stack.is_empty() {
                            match key.strip_prefix("xmlns:") {
                                Some(p) => {
                                    declared.prefixes.insert(p.to_string(), value);
                                }
                                None => declared.default = Some(value),
                            }
                        }
                    } else {
                        attrs.push((key, value));
                    }
                }
                if stack.is_empty() && namespaces.is_none() {
                    namespaces = Some(declared);
                }
                let ns = namespaces
                    .as_ref()
                    .ok_or_else(|| invalid("missing root element"))?;
                let (prefix, local) = match qname.split_once(':') {
                    Some((p, l)) => (Some(p), l),
                    None => (None, qname.as_str()),
                };
                let xsd = match ns.resolve(prefix) {
                    Some(uri) => uri == XSD_NS,
                    None if prefix.is_some() && annotation_depth.is_none() => {
                        return Err(invalid(format!(
                            "namespace prefix '{}' is not declared on xs:schema",
                            prefix.unwrap_or_default()
                        )));
                    }
                    None => false,
                };
                let node = XNode {
                    local: local.to_string(),
                    xsd,
                    attrs,
                    children: Vec::new(),
                    text: String::new(),
                };
                if annotation_depth.is_none() && xsd && node.local == "annotation" {
                    annotation_depth = Some(stack.len());
                }
                if matches!(event, Event::Start(_)) {
                    stack.push(node);
                } else {
                    if annotation_depth == Some(stack.len()) {
                        annotation_depth = None;
                    }
                    attach(&mut stack, &mut root, node)?;
                }
            }
            Event::End(_) => {
                let node = stack.pop().ok_or_else(|| invalid("unmatched end tag"))?;
                if annotation_depth == Some(stack.len()) {
                    annotation_depth = None;
                }
                attach(&mut stack, &mut root, node)?;
            }
            Event::Text(ref t) => {
                let s = t
                    .decode()
                    .map_err(|e| invalid(format!("invalid text: {e}")))?;
                push_text(&mut stack, &s)?;
            }
            Event::CData(ref c) => {
                let s = c
                    .decode()
                    .map_err(|e| invalid(format!("invalid CDATA: {e}")))?;
                push_text(&mut stack, &s)?;
            }
            Event::GeneralRef(ref r) => {
                let mut s = String::new();
                append_ref(r, &mut s).map_err(invalid)?;
                push_text(&mut stack, &s)?;
            }
            Event::DocType(_) => {
                return Err(invalid("DOCTYPE declarations are not supported"));
            }
            Event::Eof => break,
            Event::Decl(_) | Event::Comment(_) | Event::PI(_) => {}
        }
    }
    if !stack.is_empty() {
        return Err(invalid("unexpected end of document inside an open element"));
    }
    let root = root.ok_or_else(|| invalid("no root element"))?;
    let namespaces = namespaces.ok_or_else(|| invalid("no root element"))?;
    Ok((root, namespaces))
}

fn push_text(stack: &mut [XNode], s: &str) -> Result<(), SchemaError> {
    match stack.last_mut() {
        Some(node) => node.text.push_str(s),
        None if s.chars().all(is_xml_ws) => {}
        None => return Err(invalid("text outside the root element")),
    }
    Ok(())
}

// =============================================================================
// Element tree -> declarations
// =============================================================================

fn unsupported_construct(local: &str) -> SchemaError {
    let why = match local {
        "import" | "include" | "redefine" | "override" => {
            "schema composition is not supported; put every declaration in one document"
        }
        "group" | "attributeGroup" => "named groups are not supported; declare the content inline",
        "any" | "anyAttribute" => {
            "wildcards are not supported; declare the allowed elements and attributes explicitly"
        }
        "key" | "keyref" | "unique" | "selector" | "field" => {
            "identity constraints are not supported"
        }
        "complexContent" => {
            "type derivation (extension/restriction of complex types) is not supported"
        }
        "list" | "union" => "list and union simple types are not supported",
        "attribute" => {
            "global attributes are not supported; declare attributes inside a complexType"
        }
        "notation" => "notations are not supported",
        _ => "this construct is not part of the supported XSD subset",
    };
    invalid(format!("xs:{local}: {why}"))
}

fn is_ncname(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    s.chars().count() <= MAX_NAME_CHARS
        && (first.is_alphabetic() || first == '_')
        && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn required_name(node: &XNode) -> Result<String, SchemaError> {
    let name = node.attr("name").ok_or_else(|| {
        invalid(format!(
            "xs:{} needs a name{}",
            node.local,
            if node.attr("ref").is_some() {
                " (references with ref= are not supported; declare it in place)"
            } else {
                ""
            }
        ))
    })?;
    if !is_ncname(name) {
        return Err(invalid(format!(
            "'{name}' is not a valid name (letters, digits, '_', '-', '.'; at most \
             {MAX_NAME_CHARS} characters)"
        )));
    }
    Ok(name.to_string())
}

fn parse_occurrence_value(v: &str, what: &str) -> Result<u32, SchemaError> {
    let n: u32 = v
        .parse()
        .map_err(|_| invalid(format!("{what} '{v}' is not a non-negative integer")))?;
    if n > MAX_OCCURS {
        return Err(invalid(format!(
            "{what} {n} exceeds the limit of {MAX_OCCURS}"
        )));
    }
    Ok(n)
}

fn parse_occurs(node: &XNode) -> Result<(u32, Max), SchemaError> {
    let min = match node.attr("minOccurs") {
        None => 1,
        Some(v) => parse_occurrence_value(v, "minOccurs")?,
    };
    let max = match node.attr("maxOccurs") {
        None => Max::Bounded(1),
        Some("unbounded") => Max::Unbounded,
        Some(v) => Max::Bounded(parse_occurrence_value(v, "maxOccurs")?),
    };
    if let Max::Bounded(m) = max {
        if m < min {
            return Err(invalid(format!(
                "xs:{}: minOccurs {min} is greater than maxOccurs {m}",
                node.local
            )));
        }
    }
    Ok((min, max))
}

/// The schema's children without `annotation`s, each checked to be an XSD
/// element, and the node's own text checked to be blank.
fn content_children(node: &XNode) -> Result<Vec<&XNode>, SchemaError> {
    if !node.text.chars().all(is_xml_ws) {
        return Err(invalid(format!("xs:{} must not contain text", node.local)));
    }
    let mut out = Vec::new();
    for child in &node.children {
        if !child.xsd {
            return Err(invalid(format!(
                "element '{}' is not in the XML Schema namespace",
                child.local
            )));
        }
        if child.local != "annotation" {
            out.push(child);
        }
    }
    Ok(out)
}

/// First non-empty `annotation/documentation` text directly under `node`.
fn documentation(node: &XNode) -> Option<String> {
    node.children
        .iter()
        .filter(|c| c.xsd && c.local == "annotation")
        .flat_map(|a| a.children.iter())
        .filter(|d| d.xsd && d.local == "documentation")
        .map(|d| d.text.trim())
        .find(|t| !t.is_empty())
        .map(str::to_string)
}

struct Interpreter<'n> {
    ns: &'n Namespaces,
    target_ns: Option<&'n str>,
}

impl Interpreter<'_> {
    fn resolve_type(&self, qname: &str) -> Result<TypeUse, SchemaError> {
        let (prefix, local) = match qname.split_once(':') {
            Some((p, l)) => (Some(p), l),
            None => (None, qname),
        };
        if prefix.is_some() && self.ns.resolve(prefix).is_none() {
            return Err(invalid(format!(
                "type '{qname}' uses a namespace prefix that is not declared on xs:schema"
            )));
        }
        match self.ns.resolve(prefix) {
            Some(XSD_NS) => Builtin::from_local(local)
                .map(TypeUse::Builtin)
                .ok_or_else(|| {
                    invalid(format!(
                        "built-in type xs:{local} is not supported (supported: {SUPPORTED_BUILTINS})"
                    ))
                }),
            ns if ns.is_none() || ns == self.target_ns => Ok(TypeUse::Named(local.to_string())),
            Some(other) => Err(invalid(format!(
                "type '{qname}' belongs to namespace '{other}'; types from other namespaces \
                 are not supported"
            ))),
            None => unreachable!("covered by the guard above"),
        }
    }

    fn simple_base(&self, qname: &str) -> Result<SimpleBase, SchemaError> {
        match self.resolve_type(qname)? {
            TypeUse::Builtin(b) => Ok(SimpleBase::Builtin(b)),
            TypeUse::Named(n) => Ok(SimpleBase::Named(n)),
            _ => unreachable!("resolve_type returns only builtin or named"),
        }
    }

    fn schema(&self, root: &XNode) -> Result<Doc, SchemaError> {
        let mut doc = Doc {
            namespaced: self.target_ns.is_some(),
            description: documentation(root),
            elements: Vec::new(),
            simple_types: Vec::new(),
            complex_types: Vec::new(),
        };
        for child in content_children(root)? {
            match child.local.as_str() {
                "element" => doc.elements.push(self.element(child, true)?),
                "complexType" => {
                    let name = required_name(child)?;
                    doc.complex_types
                        .push((name, self.complex_type(child, true)?));
                }
                "simpleType" => {
                    let name = required_name(child)?;
                    doc.simple_types
                        .push((name, self.simple_type(child, true)?));
                }
                other => return Err(unsupported_construct(other)),
            }
        }
        Ok(doc)
    }

    fn element(&self, node: &XNode, global: bool) -> Result<ElementDef, SchemaError> {
        node.check_attrs(&["name", "type", "minOccurs", "maxOccurs"])?;
        let name = required_name(node)?;
        let (min, max) = if global {
            if node.attr("minOccurs").is_some() || node.attr("maxOccurs").is_some() {
                return Err(invalid(format!(
                    "global element '{name}' must not carry minOccurs/maxOccurs"
                )));
            }
            (1, Max::Bounded(1))
        } else {
            parse_occurs(node)?
        };
        let mut inline: Option<TypeUse> = None;
        for child in content_children(node)? {
            match child.local.as_str() {
                "complexType" if inline.is_none() => {
                    inline = Some(TypeUse::Complex(Box::new(self.complex_type(child, false)?)));
                }
                "simpleType" if inline.is_none() => {
                    inline = Some(TypeUse::Simple(Box::new(self.simple_type(child, false)?)));
                }
                "complexType" | "simpleType" => {
                    return Err(invalid(format!(
                        "element '{name}' declares more than one inline type"
                    )));
                }
                other => return Err(unsupported_construct(other)),
            }
        }
        let ty = match (node.attr("type"), inline) {
            (Some(_), Some(_)) => {
                return Err(invalid(format!(
                    "element '{name}' has both a type attribute and an inline type"
                )));
            }
            (Some(t), None) => self.resolve_type(t)?,
            (None, Some(t)) => t,
            (None, None) => {
                return Err(invalid(format!(
                    "element '{name}' has no type; an untyped element is xs:anyType, which is \
                     not supported - declare its type"
                )));
            }
        };
        Ok(ElementDef { name, ty, min, max })
    }

    fn complex_type(&self, node: &XNode, named: bool) -> Result<ComplexDef, SchemaError> {
        node.check_attrs(if named {
            &["name", "mixed"]
        } else {
            &["mixed"]
        })?;
        if node.attr("mixed").is_some_and(|m| m != "false") {
            return Err(invalid("mixed content (mixed=\"true\") is not supported"));
        }
        let mut content = ComplexContent::Empty;
        let mut have_content = false;
        let mut attributes: Vec<AttrDef> = Vec::new();
        for child in content_children(node)? {
            match child.local.as_str() {
                "sequence" | "choice" | "all" if !have_content && attributes.is_empty() => {
                    content = ComplexContent::Group(self.group(child, true)?);
                    have_content = true;
                }
                "simpleContent" if !have_content && attributes.is_empty() => {
                    let (base, attrs) = self.simple_content(child)?;
                    content = ComplexContent::Text(base);
                    attributes.extend(attrs);
                    have_content = true;
                }
                "sequence" | "choice" | "all" | "simpleContent" => {
                    return Err(invalid(
                        "a complexType takes exactly one content model, declared before its \
                         attributes",
                    ));
                }
                "attribute" => attributes.push(self.attribute(child)?),
                other => return Err(unsupported_construct(other)),
            }
        }
        let mut seen = HashSet::new();
        for a in &attributes {
            if !seen.insert(a.name.as_str()) {
                return Err(invalid(format!("attribute '{}' is declared twice", a.name)));
            }
        }
        Ok(ComplexDef {
            content,
            attributes,
        })
    }

    fn simple_content(&self, node: &XNode) -> Result<(SimpleBase, Vec<AttrDef>), SchemaError> {
        node.check_attrs(&[])?;
        let children = content_children(node)?;
        let [ext] = children.as_slice() else {
            return Err(invalid("xs:simpleContent takes exactly one xs:extension"));
        };
        if ext.local != "extension" {
            return Err(invalid(format!(
                "xs:simpleContent/xs:{} is not supported; use xs:extension",
                ext.local
            )));
        }
        ext.check_attrs(&["base"])?;
        let base = self.simple_base(
            ext.attr("base")
                .ok_or_else(|| invalid("xs:extension needs a base"))?,
        )?;
        let mut attrs = Vec::new();
        for child in content_children(ext)? {
            if child.local != "attribute" {
                return Err(unsupported_construct(&child.local));
            }
            attrs.push(self.attribute(child)?);
        }
        Ok((base, attrs))
    }

    fn group(&self, node: &XNode, top: bool) -> Result<GroupDef, SchemaError> {
        node.check_attrs(&["minOccurs", "maxOccurs"])?;
        let kind = match node.local.as_str() {
            "sequence" => GroupKind::Sequence,
            "choice" => GroupKind::Choice,
            _ => GroupKind::All,
        };
        let (min, max) = parse_occurs(node)?;
        if kind == GroupKind::All && (!top || min > 1 || max != Max::Bounded(1)) {
            return Err(invalid(
                "xs:all must be the top-level group of a complexType and may occur at most once",
            ));
        }
        let mut items = Vec::new();
        let mut prohibited = Vec::new();
        for child in content_children(node)? {
            match child.local.as_str() {
                "element" => {
                    let el = self.element(child, false)?;
                    if kind == GroupKind::All {
                        // maxOccurs="0" prohibits the element: it must never
                        // match, so it is not an item of the group.
                        if el.max == Max::Bounded(0) {
                            prohibited.push(el);
                            continue;
                        }
                        if el.max != Max::Bounded(1) {
                            return Err(invalid(format!(
                                "element '{}' inside xs:all may occur at most once",
                                el.name
                            )));
                        }
                    }
                    items.push(Item::Element(el));
                }
                "sequence" | "choice" if kind != GroupKind::All => {
                    items.push(Item::Group(self.group(child, false)?));
                }
                "sequence" | "choice" | "all" => {
                    return Err(invalid(format!(
                        "xs:{} cannot be nested here (xs:all holds elements only and cannot be \
                         nested)",
                        child.local
                    )));
                }
                other => return Err(unsupported_construct(other)),
            }
        }
        if kind == GroupKind::Choice && items.is_empty() {
            return Err(invalid("xs:choice must contain at least one particle"));
        }
        Ok(GroupDef {
            kind,
            min,
            max,
            items,
            prohibited,
        })
    }

    fn attribute(&self, node: &XNode) -> Result<AttrDef, SchemaError> {
        node.check_attrs(&["name", "type", "use"])?;
        let name = required_name(node)?;
        let required = match node.attr("use") {
            None | Some("optional") => false,
            Some("required") => true,
            Some(other) => {
                return Err(invalid(format!(
                    "attribute '{name}': use=\"{other}\" is not supported (required or optional)"
                )));
            }
        };
        let mut inline = None;
        for child in content_children(node)? {
            if child.local != "simpleType" || inline.is_some() {
                return Err(unsupported_construct(&child.local));
            }
            inline = Some(TypeUse::Simple(Box::new(self.simple_type(child, false)?)));
        }
        let ty = match (node.attr("type"), inline) {
            (Some(_), Some(_)) => {
                return Err(invalid(format!(
                    "attribute '{name}' has both a type attribute and an inline type"
                )));
            }
            (Some(t), None) => self.resolve_type(t)?,
            (None, Some(t)) => t,
            // An attribute without a type is xs:anySimpleType: any text.
            (None, None) => TypeUse::Builtin(Builtin::String),
        };
        Ok(AttrDef { name, ty, required })
    }

    fn simple_type(&self, node: &XNode, named: bool) -> Result<SimpleDef, SchemaError> {
        node.check_attrs(if named { &["name"] } else { &[] })?;
        let children = content_children(node)?;
        let [restriction] = children.as_slice() else {
            return Err(invalid("xs:simpleType takes exactly one xs:restriction"));
        };
        if restriction.local != "restriction" {
            return Err(unsupported_construct(&restriction.local));
        }
        restriction.check_attrs(&["base"])?;
        let base = self.simple_base(
            restriction
                .attr("base")
                .ok_or_else(|| invalid("xs:restriction needs a base"))?,
        )?;
        let mut facets = FacetDef::default();
        for facet in content_children(restriction)? {
            facet.check_attrs(&["value"])?;
            if !facet.children.iter().all(|c| c.local == "annotation") {
                return Err(invalid(format!("xs:{} must be empty", facet.local)));
            }
            let value = facet
                .attr("value")
                .ok_or_else(|| invalid(format!("xs:{} needs a value", facet.local)))?;
            match facet.local.as_str() {
                "minLength" | "maxLength" => {
                    let slot = if facet.local == "minLength" {
                        &mut facets.min_length
                    } else {
                        &mut facets.max_length
                    };
                    if slot.is_some() {
                        return Err(invalid(format!("xs:{} is given twice", facet.local)));
                    }
                    *slot = Some(value.parse().map_err(|_| {
                        invalid(format!(
                            "xs:{} value '{value}' is not a non-negative integer",
                            facet.local
                        ))
                    })?);
                }
                "pattern" => facets.patterns.push(value.to_string()),
                "enumeration" => facets.enumeration.push(value.to_string()),
                other => {
                    return Err(invalid(format!(
                        "facet xs:{other} is not supported (supported: minLength, maxLength, \
                         pattern, enumeration)"
                    )));
                }
            }
        }
        Ok(SimpleDef { base, facets })
    }
}

fn parse_doc(schema_text: &str) -> Result<Doc, SchemaError> {
    if schema_text.len() > MAX_SCHEMA_TEXT_BYTES {
        return Err(invalid(format!(
            "schema text is {} bytes, exceeding the {MAX_SCHEMA_TEXT_BYTES}-byte limit",
            schema_text.len()
        )));
    }
    let (root, ns) = build_tree(schema_text)?;
    if !(root.xsd && root.local == "schema") {
        return Err(invalid("the root element must be xs:schema"));
    }
    root.check_attrs(&[
        "targetNamespace",
        "elementFormDefault",
        "attributeFormDefault",
        "version",
        "xml:lang",
    ])?;
    let interpreter = Interpreter {
        ns: &ns,
        target_ns: root.attr("targetNamespace"),
    };
    let doc = interpreter.schema(&root)?;
    if doc.elements.is_empty() {
        return Err(invalid(
            "the schema declares no global element; at least one document root is required",
        ));
    }
    Ok(doc)
}

// =============================================================================
// Compiled schema
// =============================================================================

#[derive(Clone, Debug)]
struct Pattern {
    source: String,
    matcher: Arc<Matcher>,
}

#[derive(Clone, Debug, Default)]
struct Facets {
    min_length: Option<u32>,
    max_length: Option<u32>,
    patterns: Vec<Pattern>,
    /// Canonical members; empty means "no enumeration".
    enumeration: HashSet<String>,
}

impl Facets {
    fn is_unconstrained(&self) -> bool {
        self.min_length.is_none()
            && self.max_length.is_none()
            && self.patterns.is_empty()
            && self.enumeration.is_empty()
    }
}

/// Why [`SimpleType::check`] stopped.
#[derive(Debug, PartialEq, Eq)]
enum CheckFailure {
    /// The value breaks the named constraint.
    Violated(String),
    /// The budget ran out; the value is not known to be valid or invalid.
    TooComplex,
}

impl From<LimitExceeded> for CheckFailure {
    fn from(_: LimitExceeded) -> CheckFailure {
        CheckFailure::TooComplex
    }
}

fn violated(msg: impl Into<String>) -> CheckFailure {
    CheckFailure::Violated(msg.into())
}

#[derive(Clone, Debug)]
struct SimpleType {
    builtin: Builtin,
    /// One entry per restriction step, base-most first; all must hold.
    steps: Vec<Arc<Facets>>,
}

/// Each restriction step's pattern sources, sorted, so two steps compare as
/// sets.
fn sorted_sources(t: &SimpleType) -> Vec<Vec<&str>> {
    t.steps
        .iter()
        .map(|s| {
            let mut list: Vec<&str> = s.patterns.iter().map(|p| p.source.as_str()).collect();
            list.sort_unstable();
            list
        })
        .collect()
}

impl SimpleType {
    /// `Violated` names the broken constraint, never the value. Every scan of
    /// the value is charged to `budget` BEFORE it runs.
    fn check(&self, raw: &str, budget: &mut Budget) -> Result<(), CheckFailure> {
        let value = if self.builtin.is_string() {
            raw
        } else {
            raw.trim_matches(is_xml_ws)
        };
        let len = value.len() as u64;
        // A string has no lexical form to scan.
        budget.charge(if self.builtin.is_string() { 1 } else { 1 + len })?;
        if !valid_lexical(self.builtin, value) {
            return Err(violated(format!(
                "is not a valid xs:{}",
                self.builtin.name()
            )));
        }
        let mut canonical_value: Option<String> = None;
        for step in &self.steps {
            if step.min_length.is_some() || step.max_length.is_some() {
                budget.charge(len)?;
                let count = value.chars().count();
                if step.min_length.is_some_and(|m| count < m as usize) {
                    return Err(violated("is shorter than minLength"));
                }
                if step.max_length.is_some_and(|m| count > m as usize) {
                    return Err(violated("is longer than maxLength"));
                }
            }
            if !step.patterns.is_empty() {
                let mut matched = false;
                for p in &step.patterns {
                    if p.matcher.is_match(value, budget)? {
                        matched = true;
                        break;
                    }
                }
                if !matched {
                    return Err(violated("does not match the pattern"));
                }
            }
            if !step.enumeration.is_empty() {
                if canonical_value.is_none() {
                    budget.charge(len)?;
                }
                let c = canonical_value.get_or_insert_with(|| canonical(self.builtin, value));
                budget.charge(1 + len)?;
                if !step.enumeration.contains(c.as_str()) {
                    return Err(violated("is not one of the enumerated values"));
                }
            }
        }
        Ok(())
    }

    /// Enumeration every value must come from: the intersection of every
    /// step's enumeration, or `None` when no step has one. Sorted, so a
    /// message naming a member is deterministic. Each intersection is charged
    /// before it is computed.
    fn effective_enumeration(
        &self,
        budget: &mut Budget,
    ) -> Result<Option<Vec<&String>>, LimitExceeded> {
        let mut result: Option<Vec<&String>> = None;
        for step in self.steps.iter().filter(|s| !s.enumeration.is_empty()) {
            result = Some(match result {
                None => {
                    budget.charge(step.enumeration.len() as u64)?;
                    step.enumeration.iter().collect()
                }
                Some(prev) => {
                    budget.charge((prev.len() + step.enumeration.len()) as u64)?;
                    prev.into_iter()
                        .filter(|m| step.enumeration.contains(m.as_str()))
                        .collect()
                }
            });
        }
        if let Some(members) = result.as_mut() {
            let n = members.len() as u64;
            budget.charge(n.saturating_mul(u64::from(u64::BITS - n.leading_zeros())))?;
            members.sort_unstable();
        }
        Ok(result)
    }

    fn effective_length(&self) -> (u32, Option<u32>) {
        let min = self
            .steps
            .iter()
            .filter_map(|s| s.min_length)
            .max()
            .unwrap_or(0);
        let max = self.steps.iter().filter_map(|s| s.max_length).min();
        (min, max)
    }
}

#[derive(Debug)]
struct ElementDecl {
    ty: usize,
}

#[derive(Debug)]
struct AttrDecl {
    name: String,
    /// Index of a `TypeDef::Simple`.
    ty: usize,
    required: bool,
}

#[derive(Debug)]
struct ChildDecl {
    name: String,
    element: usize,
}

#[derive(Debug, Default)]
struct NfaState {
    eps: Vec<u32>,
    edge: Option<(u32, u32)>,
}

/// The accepting state of every [`Nfa`]. It is state 0 and a state set keeps it
/// first (the closure moves it there and the sorted sets of a comparison start
/// with the smallest id), so "does this set accept" reads one element.
const NFA_ACCEPT: u32 = 0;

#[derive(Debug)]
struct Nfa {
    states: Vec<NfaState>,
    start_set: Vec<u32>,
}

#[derive(Debug)]
struct NfaModel {
    children: Vec<ChildDecl>,
    by_name: HashMap<String, u32>,
    nfa: Nfa,
}

#[derive(Debug)]
struct AllModel {
    children: Vec<ChildDecl>,
    required: Vec<bool>,
    /// Whether any child is required, so an empty group is judged in O(1).
    any_required: bool,
    by_name: HashMap<String, u32>,
    /// `minOccurs="0"` on the group: the whole group may be absent.
    optional: bool,
}

impl AllModel {
    fn accepts_empty(&self) -> bool {
        self.optional || !self.any_required
    }
}

#[derive(Debug)]
enum Model {
    Nfa(NfaModel),
    All(AllModel),
}

#[derive(Debug)]
enum Content {
    Empty,
    /// Index of a `TypeDef::Simple`.
    Text(usize),
    Model(Model),
}

/// The attributes of one complex type with the lookups validation and
/// comparison need, built once at compile.
#[derive(Debug, Default)]
struct Attrs {
    decls: Vec<AttrDecl>,
    index: HashMap<String, usize>,
    required: usize,
}

impl Attrs {
    fn new(decls: Vec<AttrDecl>) -> Attrs {
        let index = decls
            .iter()
            .enumerate()
            .map(|(i, a)| (a.name.clone(), i))
            .collect();
        let required = decls.iter().filter(|a| a.required).count();
        Attrs {
            decls,
            index,
            required,
        }
    }

    fn get(&self, name: &str) -> Option<&AttrDecl> {
        self.index.get(name).map(|&i| &self.decls[i])
    }
}

#[derive(Debug)]
struct ComplexType {
    attributes: Attrs,
    content: Content,
}

#[derive(Debug)]
enum TypeDef {
    Simple(SimpleType),
    Complex(ComplexType),
}

#[derive(Debug)]
pub struct Compiled {
    elements: Vec<ElementDecl>,
    types: Vec<TypeDef>,
    roots: HashMap<String, usize>,
    max_nfa_states: usize,
    /// Pattern memory proxy, enumeration bytes and the content models'
    /// automata, for cache accounting.
    pattern_memory: usize,
    enum_bytes: usize,
    model_bytes: usize,
}

impl Compiled {
    /// Resident bytes the schema text does not show: the pattern programs and
    /// lazy-DFA caches, the enumeration sets (members are held once in the
    /// set and once as hash-table slack, hence the factor), and the content
    /// model automata and declaration tables.
    pub(super) fn extra_bytes(&self) -> usize {
        self.pattern_memory + 2 * self.enum_bytes + self.model_bytes
    }

    fn simple(&self, idx: usize) -> &SimpleType {
        match &self.types[idx] {
            TypeDef::Simple(s) => s,
            TypeDef::Complex(_) => unreachable!("compile only stores simple types here"),
        }
    }
}

// ---- NFA ----------------------------------------------------------------

enum Term {
    Element(u32),
    Sequence(Vec<Particle>),
    Choice(Vec<Particle>),
}

struct Particle {
    term: Term,
    min: u32,
    max: Max,
}

#[derive(Clone, Copy)]
struct Frag {
    start: u32,
    end: u32,
}

struct NfaBuilder<'b> {
    states: Vec<NfaState>,
    limit: usize,
    total: &'b mut usize,
}

impl NfaBuilder<'_> {
    fn state(&mut self) -> Result<u32, SchemaError> {
        if self.states.len() >= self.limit || *self.total >= MAX_TOTAL_NFA_STATES {
            return Err(invalid(format!(
                "a content model is too large: occurrence counts expand to more than \
                 {MAX_NFA_STATES} states ({MAX_TOTAL_NFA_STATES} per schema); lower \
                 minOccurs/maxOccurs or use unbounded"
            )));
        }
        *self.total += 1;
        self.states.push(NfaState::default());
        Ok(self.states.len() as u32 - 1)
    }

    fn link(&mut self, from: u32, to: u32) {
        self.states[from as usize].eps.push(to);
    }

    fn empty(&mut self) -> Result<Frag, SchemaError> {
        let s = self.state()?;
        Ok(Frag { start: s, end: s })
    }

    fn once(&mut self, p: &Particle) -> Result<Frag, SchemaError> {
        match &p.term {
            Term::Element(label) => {
                let start = self.state()?;
                let end = self.state()?;
                self.states[start as usize].edge = Some((*label, end));
                Ok(Frag { start, end })
            }
            Term::Sequence(items) => {
                let mut frag = self.empty()?;
                for item in items {
                    let next = self.particle(item)?;
                    self.link(frag.end, next.start);
                    frag.end = next.end;
                }
                Ok(frag)
            }
            Term::Choice(items) => {
                let start = self.state()?;
                let end = self.state()?;
                for item in items {
                    let alt = self.particle(item)?;
                    self.link(start, alt.start);
                    self.link(alt.end, end);
                }
                Ok(Frag { start, end })
            }
        }
    }

    /// `p` repeated according to its occurrence bounds.
    fn particle(&mut self, p: &Particle) -> Result<Frag, SchemaError> {
        if p.max == Max::Bounded(0) {
            return self.empty();
        }
        let mut parts = Vec::new();
        for _ in 0..p.min {
            parts.push(self.once(p)?);
        }
        match p.max {
            Max::Unbounded => {
                let inner = self.once(p)?;
                let start = self.state()?;
                let end = self.state()?;
                self.link(start, inner.start);
                self.link(inner.end, start);
                self.link(start, end);
                parts.push(Frag { start, end });
            }
            Max::Bounded(max) => {
                for _ in p.min..max {
                    let inner = self.once(p)?;
                    let start = self.state()?;
                    let end = self.state()?;
                    self.link(start, inner.start);
                    self.link(inner.end, end);
                    self.link(start, end);
                    parts.push(Frag { start, end });
                }
            }
        }
        let mut iter = parts.into_iter();
        let Some(mut frag) = iter.next() else {
            return self.empty();
        };
        for next in iter {
            self.link(frag.end, next.start);
            frag.end = next.end;
        }
        Ok(frag)
    }
}

/// Scratch space for epsilon-closure walks; one per validation or
/// comparison so a hot loop allocates no visited set per step.
struct Marks {
    stamp: u32,
    seen: Vec<u32>,
    work: Vec<u32>,
}

impl Marks {
    fn new(states: usize) -> Marks {
        Marks {
            stamp: 0,
            seen: vec![0; states],
            work: Vec::new(),
        }
    }

    fn next_stamp(&mut self) -> u32 {
        if self.stamp == u32::MAX {
            self.seen.fill(0);
            self.stamp = 0;
        }
        self.stamp += 1;
        self.stamp
    }
}

impl Nfa {
    /// Fills the empty `out` with every state reachable from `seeds` through
    /// epsilon edges that matters for matching (it has an edge or is the
    /// accept state, which comes first). Every visited state is charged to
    /// `budget`.
    fn closure(
        &self,
        seeds: &[u32],
        out: &mut Vec<u32>,
        marks: &mut Marks,
        budget: &mut Budget,
    ) -> Result<(), LimitExceeded> {
        debug_assert!(out.is_empty());
        budget.charge(1 + seeds.len() as u64)?;
        let stamp = marks.next_stamp();
        marks.work.clear();
        for &s in seeds {
            if marks.seen[s as usize] != stamp {
                marks.seen[s as usize] = stamp;
                marks.work.push(s);
            }
        }
        while let Some(s) = marks.work.pop() {
            let state = &self.states[s as usize];
            budget.charge(1 + state.eps.len() as u64)?;
            if s == NFA_ACCEPT {
                out.push(s);
                let last = out.len() - 1;
                out.swap(0, last);
            } else if state.edge.is_some() {
                out.push(s);
            }
            for &t in &state.eps {
                if marks.seen[t as usize] != stamp {
                    marks.seen[t as usize] = stamp;
                    marks.work.push(t);
                }
            }
        }
        Ok(())
    }

    fn step(
        &self,
        active: &[u32],
        label: u32,
        marks: &mut Marks,
        budget: &mut Budget,
    ) -> Result<Vec<u32>, LimitExceeded> {
        budget.charge(active.len() as u64)?;
        let seeds: Vec<u32> = active
            .iter()
            .filter_map(|&s| match self.states[s as usize].edge {
                Some((l, to)) if l == label => Some(to),
                _ => None,
            })
            .collect();
        let mut out = Vec::new();
        self.closure(&seeds, &mut out, marks, budget)?;
        Ok(out)
    }

    fn accepts(&self, active: &[u32]) -> bool {
        active.first() == Some(&NFA_ACCEPT)
    }

    fn labels_from(&self, active: &[u32]) -> BTreeSet<u32> {
        active
            .iter()
            .filter_map(|&s| self.states[s as usize].edge.map(|(l, _)| l))
            .collect()
    }
}

// ---- AST -> compiled -----------------------------------------------------

struct Labels {
    children: Vec<ChildDecl>,
    by_name: HashMap<String, u32>,
}

struct Compiler<'d> {
    doc: &'d Doc,
    types: Vec<TypeDef>,
    elements: Vec<ElementDecl>,
    builtin_idx: HashMap<Builtin, usize>,
    simple_idx: HashMap<String, usize>,
    complex_idx: HashMap<String, usize>,
    simple_pending: HashSet<String>,
    /// Declarations by name, so a reference resolves in O(1) instead of a
    /// scan of every declaration.
    simple_decl: HashMap<&'d str, &'d SimpleDef>,
    complex_decl: HashSet<&'d str>,
    patterns: usize,
    pattern_memory: usize,
    enum_values: usize,
    enum_bytes: usize,
    model_bytes: usize,
    nfa_states: usize,
    max_nfa_states: usize,
    work: Budget,
}

impl<'d> Compiler<'d> {
    fn new(doc: &'d Doc) -> Compiler<'d> {
        Compiler {
            doc,
            types: Vec::new(),
            elements: Vec::new(),
            builtin_idx: HashMap::new(),
            simple_idx: HashMap::new(),
            complex_idx: HashMap::new(),
            simple_pending: HashSet::new(),
            simple_decl: HashMap::new(),
            complex_decl: HashSet::new(),
            patterns: 0,
            pattern_memory: 0,
            enum_values: 0,
            enum_bytes: 0,
            model_bytes: 0,
            nfa_states: 0,
            max_nfa_states: 1,
            work: Budget::new(MAX_COMPILE_WORK),
        }
    }

    fn charge(&mut self, units: u64) -> Result<(), SchemaError> {
        self.work
            .charge(units)
            .map_err(|_| invalid(COMPILE_WORK_REFUSAL))
    }

    fn run(mut self) -> Result<Compiled, SchemaError> {
        let doc = self.doc;
        let mut names = HashSet::new();
        for (name, def) in &doc.simple_types {
            if !names.insert(name.as_str()) {
                return Err(invalid(format!("type '{name}' is declared twice")));
            }
            self.simple_decl.insert(name.as_str(), def);
        }
        for (name, _) in &doc.complex_types {
            if !names.insert(name.as_str()) {
                return Err(invalid(format!("type '{name}' is declared twice")));
            }
            self.complex_decl.insert(name.as_str());
        }
        let mut roots_seen = HashSet::new();
        for e in &doc.elements {
            if !roots_seen.insert(e.name.as_str()) {
                return Err(invalid(format!(
                    "global element '{}' is declared twice",
                    e.name
                )));
            }
        }

        // Every named simple type is compiled even if unreferenced: nothing
        // in a registered schema may hide unchecked.
        for (name, _) in &doc.simple_types {
            self.named_simple(name)?;
        }
        for (name, _) in &doc.complex_types {
            self.types.push(TypeDef::Complex(ComplexType {
                attributes: Attrs::default(),
                content: Content::Empty,
            }));
            self.complex_idx.insert(name.clone(), self.types.len() - 1);
        }
        for (name, def) in &doc.complex_types {
            let compiled = self.complex_def(def)?;
            let idx = self.complex_idx[name];
            self.types[idx] = TypeDef::Complex(compiled);
        }
        let mut roots = HashMap::new();
        for e in &doc.elements {
            let idx = self.element(e)?;
            roots.insert(e.name.clone(), idx);
        }
        Ok(Compiled {
            roots,
            max_nfa_states: self.max_nfa_states,
            pattern_memory: self.pattern_memory,
            enum_bytes: self.enum_bytes,
            model_bytes: self.model_bytes
                + self.types.len() * std::mem::size_of::<TypeDef>()
                + self.elements.len() * std::mem::size_of::<ElementDecl>(),
            types: self.types,
            elements: self.elements,
        })
    }

    fn push_type(&mut self, t: TypeDef) -> usize {
        self.types.push(t);
        self.types.len() - 1
    }

    fn builtin(&mut self, b: Builtin) -> usize {
        if let Some(&i) = self.builtin_idx.get(&b) {
            return i;
        }
        let i = self.push_type(TypeDef::Simple(SimpleType {
            builtin: b,
            steps: Vec::new(),
        }));
        self.builtin_idx.insert(b, i);
        i
    }

    fn named_simple(&mut self, name: &str) -> Result<usize, SchemaError> {
        if let Some(&i) = self.simple_idx.get(name) {
            return Ok(i);
        }
        if !self.simple_pending.insert(name.to_string()) {
            return Err(invalid(format!(
                "simple type '{name}' is defined in terms of itself"
            )));
        }
        if self.simple_pending.len() > MAX_SCHEMA_DEPTH {
            return Err(invalid(format!(
                "simple type '{name}' is derived through a chain of more than \
                 {MAX_SCHEMA_DEPTH} named types"
            )));
        }
        let def = *self
            .simple_decl
            .get(name)
            .ok_or_else(|| invalid(format!("simple type '{name}' is not declared")))?;
        let compiled = self.simple_def(def, name)?;
        self.simple_pending.remove(name);
        let idx = self.push_type(TypeDef::Simple(compiled));
        self.simple_idx.insert(name.to_string(), idx);
        Ok(idx)
    }

    fn simple_base(&mut self, base: &SimpleBase) -> Result<SimpleType, SchemaError> {
        match base {
            SimpleBase::Builtin(b) => Ok(SimpleType {
                builtin: *b,
                steps: Vec::new(),
            }),
            SimpleBase::Named(n) => {
                let idx = self.simple_type_idx(n)?;
                Ok(self.simple_of(idx))
            }
        }
    }

    fn simple_of(&self, idx: usize) -> SimpleType {
        match &self.types[idx] {
            TypeDef::Simple(s) => s.clone(),
            TypeDef::Complex(_) => unreachable!("indexes handed out here are simple"),
        }
    }

    /// Index of the named SIMPLE type `n`; a complex or unknown name is an
    /// error naming the problem.
    fn simple_type_idx(&mut self, n: &str) -> Result<usize, SchemaError> {
        if self.simple_decl.contains_key(n) {
            self.named_simple(n)
        } else if self.complex_decl.contains(n) {
            Err(invalid(format!(
                "'{n}' is a complex type; a simple type is required here"
            )))
        } else {
            Err(invalid(format!("unknown type '{n}'")))
        }
    }

    fn simple_def(&mut self, def: &SimpleDef, what: &str) -> Result<SimpleType, SchemaError> {
        let mut st = self.simple_base(&def.base)?;
        let f = &def.facets;
        if (f.min_length.is_some() || f.max_length.is_some()) && !st.builtin.is_string() {
            return Err(invalid(format!(
                "type '{what}': minLength/maxLength apply to xs:string only, not xs:{}",
                st.builtin.name()
            )));
        }
        if let (Some(min), Some(max)) = (f.min_length, f.max_length) {
            if min > max {
                return Err(invalid(format!(
                    "type '{what}': minLength {min} is greater than maxLength {max}"
                )));
            }
        }
        self.patterns += f.patterns.len();
        if self.patterns > MAX_PATTERNS {
            return Err(invalid(format!(
                "the schema uses more than {MAX_PATTERNS} patterns"
            )));
        }
        let mut patterns = Vec::new();
        for source in &f.patterns {
            self.charge(1 + source.len() as u64)?;
            let matcher =
                build_pattern(source).map_err(|e| invalid(format!("type '{what}': {e}")))?;
            self.pattern_memory += matcher.memory_bytes();
            if self.pattern_memory > MAX_PATTERN_MEMORY {
                return Err(invalid(format!(
                    "the patterns of the schema need more than {} KiB of memory in total",
                    MAX_PATTERN_MEMORY / 1024
                )));
            }
            patterns.push(Pattern {
                source: source.clone(),
                matcher: Arc::new(matcher),
            });
        }
        self.enum_values += f.enumeration.len();
        if self.enum_values > MAX_ENUMERATION_VALUES {
            return Err(invalid(format!(
                "the schema lists more than {MAX_ENUMERATION_VALUES} enumeration values"
            )));
        }
        let mut enumeration = HashSet::new();
        for member in &f.enumeration {
            if member.len() > MAX_ENUM_VALUE_BYTES {
                return Err(invalid(format!(
                    "type '{what}': an enumeration value is longer than {MAX_ENUM_VALUE_BYTES} bytes"
                )));
            }
            self.enum_bytes += member.len();
            if self.enum_bytes > MAX_ENUM_TOTAL_BYTES {
                return Err(invalid(format!(
                    "the enumeration values of the schema hold more than {} KiB in total",
                    MAX_ENUM_TOTAL_BYTES / 1024
                )));
            }
            self.charge(1 + member.len() as u64)?;
            let value = if st.builtin.is_string() {
                member.as_str()
            } else {
                member.trim_matches(is_xml_ws)
            };
            if !valid_lexical(st.builtin, value) {
                return Err(invalid(format!(
                    "type '{what}': enumeration value '{member}' is not a valid xs:{}",
                    st.builtin.name()
                )));
            }
            enumeration.insert(canonical(st.builtin, value));
        }
        let step = Facets {
            min_length: f.min_length,
            max_length: f.max_length,
            patterns,
            enumeration,
        };
        if !step.is_unconstrained() {
            st.steps.push(Arc::new(step));
        }
        // Steps are shared with the base, so a derived type costs one pointer
        // per step; the depth cap still bounds the chain a lookup walks.
        if st.steps.len() > MAX_SCHEMA_DEPTH {
            return Err(invalid(format!(
                "type '{what}': restrictions are stacked through more than \
                 {MAX_SCHEMA_DEPTH} derived types"
            )));
        }
        Ok(st)
    }

    fn type_use(&mut self, t: &TypeUse, what: &str) -> Result<usize, SchemaError> {
        match t {
            TypeUse::Builtin(b) => Ok(self.builtin(*b)),
            TypeUse::Named(n) => {
                if self.simple_decl.contains_key(n.as_str()) {
                    self.named_simple(n)
                } else if let Some(&i) = self.complex_idx.get(n) {
                    Ok(i)
                } else {
                    Err(invalid(format!("unknown type '{n}'")))
                }
            }
            TypeUse::Simple(def) => {
                let st = self.simple_def(def, what)?;
                Ok(self.push_type(TypeDef::Simple(st)))
            }
            TypeUse::Complex(def) => {
                let ct = self.complex_def(def)?;
                Ok(self.push_type(TypeDef::Complex(ct)))
            }
        }
    }

    fn simple_use(&mut self, t: &TypeUse, what: &str) -> Result<usize, SchemaError> {
        match t {
            TypeUse::Named(n) => self.simple_type_idx(n),
            TypeUse::Complex(_) => Err(invalid(format!("{what} needs a simple type"))),
            other => self.type_use(other, what),
        }
    }

    fn simple_base_idx(&mut self, base: &SimpleBase) -> Result<usize, SchemaError> {
        match base {
            SimpleBase::Builtin(b) => Ok(self.builtin(*b)),
            SimpleBase::Named(n) => self.simple_type_idx(n),
        }
    }

    fn element(&mut self, e: &ElementDef) -> Result<usize, SchemaError> {
        self.charge(1)?;
        let ty = self.type_use(&e.ty, &format!("element '{}'", e.name))?;
        self.elements.push(ElementDecl { ty });
        Ok(self.elements.len() - 1)
    }

    fn complex_def(&mut self, def: &ComplexDef) -> Result<ComplexType, SchemaError> {
        if def.attributes.len() > MAX_ATTRIBUTES_PER_TYPE {
            return Err(invalid(format!(
                "a type declares more than {MAX_ATTRIBUTES_PER_TYPE} attributes"
            )));
        }
        self.charge(1 + def.attributes.len() as u64)?;
        let mut attributes = Vec::new();
        for a in &def.attributes {
            let ty = self.simple_use(&a.ty, &format!("attribute '{}'", a.name))?;
            attributes.push(AttrDecl {
                name: a.name.clone(),
                ty,
                required: a.required,
            });
        }
        let content = match &def.content {
            ComplexContent::Empty => Content::Empty,
            ComplexContent::Text(base) => Content::Text(self.simple_base_idx(base)?),
            ComplexContent::Group(g) => Content::Model(self.model(g)?),
        };
        Ok(ComplexType {
            attributes: Attrs::new(attributes),
            content,
        })
    }

    fn model(&mut self, g: &GroupDef) -> Result<Model, SchemaError> {
        let mut labels = Labels {
            children: Vec::new(),
            by_name: HashMap::new(),
        };
        if g.kind == GroupKind::All {
            let mut required = Vec::new();
            for e in &g.prohibited {
                self.element(e)?;
            }
            for item in &g.items {
                let Item::Element(e) = item else {
                    return Err(invalid("xs:all can hold elements only"));
                };
                self.label(e, &mut labels)?;
                required.push(e.min >= 1);
            }
            self.model_bytes += labels_bytes(&labels) + required.len();
            return Ok(Model::All(AllModel {
                children: labels.children,
                any_required: required.iter().any(|r| *r),
                required,
                by_name: labels.by_name,
                optional: g.min == 0,
            }));
        }
        let particle = self.particle(g, &mut labels)?;
        let mut builder = NfaBuilder {
            states: Vec::new(),
            // The accept state is bookkeeping, not part of the content model.
            limit: MAX_NFA_STATES + 1,
            total: &mut self.nfa_states,
        };
        let accept = builder.state()?;
        debug_assert_eq!(accept, NFA_ACCEPT);
        let frag = builder.particle(&particle)?;
        builder.link(frag.end, accept);
        let states = builder.states;
        self.max_nfa_states = self.max_nfa_states.max(states.len());
        let mut nfa = Nfa {
            states,
            start_set: Vec::new(),
        };
        self.charge(nfa.states.len() as u64)?;
        let mut marks = Marks::new(nfa.states.len());
        let mut start_set = Vec::new();
        nfa.closure(&[frag.start], &mut start_set, &mut marks, &mut self.work)
            .map_err(|_| invalid(COMPILE_WORK_REFUSAL))?;
        start_set.sort_unstable();
        nfa.start_set = start_set;
        self.model_bytes += nfa_bytes(&nfa) + labels_bytes(&labels);
        Ok(Model::Nfa(NfaModel {
            children: labels.children,
            by_name: labels.by_name,
            nfa,
        }))
    }

    fn label(&mut self, e: &ElementDef, labels: &mut Labels) -> Result<u32, SchemaError> {
        if labels.by_name.contains_key(&e.name) {
            return Err(invalid(format!(
                "element '{}' appears more than once in one content model; give each child \
                 element of a type one position",
                e.name
            )));
        }
        let element = self.element(e)?;
        let label = labels.children.len() as u32;
        labels.children.push(ChildDecl {
            name: e.name.clone(),
            element,
        });
        labels.by_name.insert(e.name.clone(), label);
        Ok(label)
    }

    fn particle(&mut self, g: &GroupDef, labels: &mut Labels) -> Result<Particle, SchemaError> {
        let mut parts = Vec::new();
        for item in &g.items {
            parts.push(match item {
                Item::Element(e) => {
                    let label = self.label(e, labels)?;
                    Particle {
                        term: Term::Element(label),
                        min: e.min,
                        max: e.max,
                    }
                }
                Item::Group(sub) => self.particle(sub, labels)?,
            });
        }
        Ok(Particle {
            term: match g.kind {
                GroupKind::Choice => Term::Choice(parts),
                _ => Term::Sequence(parts),
            },
            min: g.min,
            max: g.max,
        })
    }
}

/// Resident bytes of a content model's automaton: the state array, each
/// state's epsilon list on the heap, and the start set.
fn nfa_bytes(nfa: &Nfa) -> usize {
    nfa.states.len() * std::mem::size_of::<NfaState>()
        + nfa
            .states
            .iter()
            .map(|s| s.eps.capacity() * std::mem::size_of::<u32>())
            .sum::<usize>()
        + nfa.start_set.capacity() * std::mem::size_of::<u32>()
}

/// Resident bytes of a content model's child table: each name is held in the
/// list and as a map key, plus the entry overhead of both.
fn labels_bytes(labels: &Labels) -> usize {
    labels
        .children
        .iter()
        .map(|c| 2 * c.name.len() + std::mem::size_of::<ChildDecl>() + 48)
        .sum()
}

fn compile_doc(doc: &Doc) -> Result<Compiled, SchemaError> {
    Compiler::new(doc).run()
}

// =============================================================================
// Validation
// =============================================================================

/// Longest element path a violation message carries. A document nested up to
/// `MAX_DOC_DEPTH` levels with long names would otherwise put kilobytes into
/// audit rows and DLQ headers.
const MAX_PATH_CHARS: usize = 256;

/// `path` unchanged when short; otherwise its first two and last two
/// segments around `…`, bounded in characters whatever the names are.
fn shorten_path(path: &str) -> String {
    if path.chars().count() <= MAX_PATH_CHARS {
        return path.to_string();
    }
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let joined = if segments.len() > 4 {
        format!(
            "/{}/{}/…/{}/{}",
            segments[0],
            segments[1],
            segments[segments.len() - 2],
            segments[segments.len() - 1]
        )
    } else {
        path.to_string()
    };
    if joined.chars().count() <= MAX_PATH_CHARS {
        return joined;
    }
    let mut cut: String = joined.chars().take(MAX_PATH_CHARS).collect();
    cut.push('…');
    cut
}

/// A payload-supplied name, cut to `MAX_NAME_CHARS` characters so a hostile
/// document cannot inflate violation text (audit rows, logs).
fn shorten_name(name: &str) -> String {
    if name.chars().count() <= MAX_NAME_CHARS {
        return name.to_string();
    }
    let mut cut: String = name.chars().take(MAX_NAME_CHARS).collect();
    cut.push('…');
    cut
}

fn violation(path: &str, msg: &str) -> SchemaError {
    SchemaError::Violation(format!("{}: {msg}", shorten_path(path)))
}

/// The check stopped before it could tell whether the document is valid.
fn limit(path: &str, msg: &str) -> SchemaError {
    SchemaError::LimitExceeded(format!("{}: {msg}", shorten_path(path)))
}

enum State {
    None,
    Nfa(Vec<u32>),
    /// Which children were seen, and whether any was.
    All(Vec<bool>, bool),
    /// Simple type index and the collected character data.
    Text(usize, String),
}

struct Frame {
    name: String,
    ty: usize,
    state: State,
}

fn path_of(stack: &[Frame], extra: Option<&str>) -> String {
    let mut p = String::new();
    for f in stack {
        p.push('/');
        p.push_str(&f.name);
    }
    if let Some(e) = extra {
        p.push('/');
        p.push_str(e);
    }
    if p.is_empty() {
        p.push_str("<root>");
    }
    p
}

/// The state of a freshly entered element. The start set and the seen-flags
/// are copied per element, so their length is charged.
fn initial_state(c: &Compiled, ty: usize, budget: &mut Budget) -> Result<State, LimitExceeded> {
    Ok(match &c.types[ty] {
        TypeDef::Simple(_) => State::Text(ty, String::new()),
        TypeDef::Complex(ct) => match &ct.content {
            Content::Empty => State::None,
            Content::Text(simple) => State::Text(*simple, String::new()),
            Content::Model(Model::Nfa(m)) => {
                budget.charge(m.nfa.start_set.len() as u64)?;
                State::Nfa(m.nfa.start_set.clone())
            }
            Content::Model(Model::All(m)) => {
                budget.charge(m.children.len() as u64)?;
                State::All(vec![false; m.children.len()], false)
            }
        },
    })
}

const VALIDATION_BUDGET_MSG: &str = "document exceeds the validation work budget";

fn check_attributes(
    c: &Compiled,
    ty: usize,
    e: &BytesStart,
    decoder: Decoder,
    budget: &mut Budget,
    path: &dyn Fn() -> String,
) -> Result<(), SchemaError> {
    let too_complex = || limit(&path(), VALIDATION_BUDGET_MSG);
    let declared: Option<&Attrs> = match &c.types[ty] {
        TypeDef::Simple(_) => None,
        TypeDef::Complex(ct) => Some(&ct.attributes),
    };
    let required_total = declared.map_or(0, |a| a.required);
    let mut required_seen = 0;
    // quick-xml's duplicate check is a linear scan of the earlier keys up to
    // 32 attributes and a hash set beyond, and none of that work reaches the
    // budget; this one is one hash insert per attribute, charged per attribute.
    let mut present: HashSet<&[u8]> = HashSet::new();
    for attr in e.attributes().with_checks(false) {
        budget.charge(COST_ATTRIBUTE).map_err(|_| too_complex())?;
        let attr = attr.map_err(|_| violation(&path(), "malformed attribute"))?;
        let key = std::str::from_utf8(attr.key.as_ref())
            .map_err(|_| violation(&path(), "attribute name is not valid UTF-8"))?;
        if !present.insert(attr.key.into_inner()) {
            return Err(violation(&path(), "malformed attribute"));
        }
        if key == "xmlns" || key.starts_with("xmlns:") {
            continue;
        }
        if let Some(local) = key.strip_prefix(XSI_PREFIX) {
            if matches!(local, "schemaLocation" | "noNamespaceSchemaLocation") {
                continue;
            }
            return Err(violation(
                &path(),
                &format!(
                    "attribute '{}' is not supported (xsi:type and xsi:nil are not honoured)",
                    shorten_name(key)
                ),
            ));
        }
        let Some(decl) = declared.and_then(|a| a.get(key)) else {
            return Err(violation(
                &path(),
                &format!("attribute '{}' is not declared", shorten_name(key)),
            ));
        };
        let value = attr
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, decoder)
            .map_err(|_| violation(&path(), "malformed attribute value"))?;
        match c.simple(decl.ty).check(&value, budget) {
            Ok(()) => {}
            Err(CheckFailure::Violated(why)) => {
                return Err(violation(
                    &path(),
                    &format!("attribute '{}' {why}", shorten_name(key)),
                ));
            }
            Err(CheckFailure::TooComplex) => return Err(too_complex()),
        }
        if decl.required {
            required_seen += 1;
        }
    }
    if required_seen < required_total {
        let missing = declared
            .and_then(|a| {
                a.decls
                    .iter()
                    .find(|d| d.required && !present.contains(d.name.as_bytes()))
            })
            .map_or("?", |d| d.name.as_str());
        return Err(violation(
            &path(),
            &format!("required attribute '{missing}' is missing"),
        ));
    }
    Ok(())
}

enum Refusal {
    NotAllowed,
    Repeated,
    TooComplex,
}

/// Resolves the element `name` entered under the current top frame (or as
/// the document root) to its declaration, advancing the parent's content
/// matcher.
fn enter_child(
    c: &Compiled,
    stack: &mut [Frame],
    name: &str,
    marks: &mut Marks,
    budget: &mut Budget,
) -> Result<usize, SchemaError> {
    let Some(parent_idx) = stack.len().checked_sub(1) else {
        return c.roots.get(name).copied().ok_or_else(|| {
            violation(
                "<root>",
                &format!("root element '{}' is not declared", shorten_name(name)),
            )
        });
    };
    let parent = &mut stack[parent_idx];
    let outcome = match &c.types[parent.ty] {
        TypeDef::Complex(ComplexType {
            content: Content::Model(model),
            ..
        }) => match (model, &mut parent.state) {
            (Model::Nfa(m), State::Nfa(active)) => match m.by_name.get(name) {
                None => Err(Refusal::NotAllowed),
                Some(&label) => match m.nfa.step(active, label, marks, budget) {
                    Err(LimitExceeded) => Err(Refusal::TooComplex),
                    Ok(next) if next.is_empty() => Err(Refusal::NotAllowed),
                    Ok(next) => {
                        *active = next;
                        Ok(m.children[label as usize].element)
                    }
                },
            },
            (Model::All(m), State::All(seen, any)) => match m.by_name.get(name) {
                None => Err(Refusal::NotAllowed),
                Some(&label) if seen[label as usize] => Err(Refusal::Repeated),
                Some(&label) => {
                    seen[label as usize] = true;
                    *any = true;
                    Ok(m.children[label as usize].element)
                }
            },
            _ => Err(Refusal::NotAllowed),
        },
        _ => Err(Refusal::NotAllowed),
    };
    outcome.map_err(|refusal| {
        let path = path_of(stack, Some(name));
        match refusal {
            Refusal::NotAllowed => violation(&path, "element is not allowed here"),
            Refusal::Repeated => violation(&path, "element occurs more than once"),
            Refusal::TooComplex => limit(&path, VALIDATION_BUDGET_MSG),
        }
    })
}

fn close_frame(
    c: &Compiled,
    stack: &mut Vec<Frame>,
    budget: &mut Budget,
) -> Result<(), SchemaError> {
    let Some(frame) = stack.last() else {
        return Err(violation("<root>", "unmatched end tag"));
    };
    let fail = |msg: &str| violation(&path_of(stack, None), msg);
    match (&frame.state, &c.types[frame.ty]) {
        (State::Text(simple, text), _) => match c.simple(*simple).check(text, budget) {
            Ok(()) => {}
            Err(CheckFailure::Violated(why)) => return Err(fail(&format!("value {why}"))),
            Err(CheckFailure::TooComplex) => {
                return Err(limit(&path_of(stack, None), VALIDATION_BUDGET_MSG));
            }
        },
        (
            State::Nfa(active),
            TypeDef::Complex(ComplexType {
                content: Content::Model(Model::Nfa(m)),
                ..
            }),
        ) => {
            if !m.nfa.accepts(active) {
                return Err(fail("required child elements are missing"));
            }
        }
        (
            State::All(seen, any),
            TypeDef::Complex(ComplexType {
                content: Content::Model(Model::All(m)),
                ..
            }),
        ) => {
            if *any {
                budget
                    .charge(seen.len() as u64)
                    .map_err(|_| limit(&path_of(stack, None), VALIDATION_BUDGET_MSG))?;
                if let Some(i) = (0..seen.len()).find(|&i| m.required[i] && !seen[i]) {
                    return Err(fail(&format!(
                        "required child element '{}' is missing",
                        m.children[i].name
                    )));
                }
            } else if !m.accepts_empty() {
                return Err(fail("required child elements are missing"));
            }
        }
        _ => {}
    }
    stack.pop();
    Ok(())
}

fn on_text(stack: &mut [Frame], text: &str) -> Result<(), SchemaError> {
    if let Some(Frame {
        state: State::Text(_, buf),
        ..
    }) = stack.last_mut()
    {
        buf.push_str(text);
        return Ok(());
    }
    if text.chars().all(is_xml_ws) {
        return Ok(());
    }
    let what = if stack.is_empty() {
        "character data outside the root element"
    } else {
        "character data is not allowed in element-only content"
    };
    Err(violation(&path_of(stack, None), what))
}

fn validate_with(c: &Compiled, payload: &[u8], budget: &mut Budget) -> Result<(), SchemaError> {
    let mut reader = Reader::from_reader(payload);
    let decoder = reader.decoder();
    let mut stack: Vec<Frame> = Vec::new();
    let mut root_closed = false;
    // The mark table is zeroed up front (4 bytes per state, about 8 per unit).
    budget
        .charge(c.max_nfa_states as u64 / 8)
        .map_err(|_| limit("<root>", VALIDATION_BUDGET_MSG))?;
    let mut marks = Marks::new(c.max_nfa_states);

    loop {
        let event = reader
            .read_event()
            .map_err(|_| violation(&path_of(&stack, None), "not well-formed XML"))?;
        budget
            .charge(COST_EVENT)
            .map_err(|_| limit(&path_of(&stack, None), VALIDATION_BUDGET_MSG))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                if root_closed {
                    return Err(violation("<root>", "more than one root element"));
                }
                if stack.len() >= MAX_DOC_DEPTH {
                    return Err(limit(
                        &path_of(&stack, None),
                        &format!(
                            "document is nested too deeply (more than {MAX_DOC_DEPTH} levels)"
                        ),
                    ));
                }
                let name = std::str::from_utf8(e.local_name().as_ref())
                    .map_err(|_| {
                        violation(&path_of(&stack, None), "element name is not valid UTF-8")
                    })?
                    .to_string();
                budget
                    .charge(COST_ELEMENT + name.len() as u64)
                    .map_err(|_| limit(&path_of(&stack, Some(&name)), VALIDATION_BUDGET_MSG))?;
                let element = enter_child(c, &mut stack, &name, &mut marks, budget)?;
                let ty = c.elements[element].ty;
                check_attributes(c, ty, e, decoder, budget, &|| path_of(&stack, Some(&name)))?;
                let state = initial_state(c, ty, budget)
                    .map_err(|_| limit(&path_of(&stack, Some(&name)), VALIDATION_BUDGET_MSG))?;
                stack.push(Frame { name, ty, state });
                if matches!(event, Event::Empty(_)) {
                    close_frame(c, &mut stack, budget)?;
                    root_closed = stack.is_empty();
                }
            }
            Event::End(_) => {
                if stack.is_empty() {
                    return Err(violation("<root>", "unmatched end tag"));
                }
                close_frame(c, &mut stack, budget)?;
                root_closed = stack.is_empty();
            }
            Event::Text(ref t) => {
                let s = t
                    .decode()
                    .map_err(|_| violation(&path_of(&stack, None), "invalid text"))?;
                on_text(&mut stack, &s)?;
            }
            Event::CData(ref t) => {
                let s = t
                    .decode()
                    .map_err(|_| violation(&path_of(&stack, None), "invalid CDATA"))?;
                on_text(&mut stack, &s)?;
            }
            Event::GeneralRef(ref r) => {
                let mut s = String::new();
                append_ref(r, &mut s).map_err(|m| violation(&path_of(&stack, None), &m))?;
                on_text(&mut stack, &s)?;
            }
            Event::DocType(_) => {
                return Err(violation("<root>", "DOCTYPE declarations are not allowed"));
            }
            Event::Eof => break,
            Event::Decl(_) | Event::Comment(_) | Event::PI(_) => {}
        }
    }
    if !stack.is_empty() {
        return Err(violation(
            &path_of(&stack, None),
            "document ends inside an open element",
        ));
    }
    if !root_closed {
        return Err(violation("<root>", "document has no root element"));
    }
    Ok(())
}

// =============================================================================
// derive_subschema
// =============================================================================

/// A policy field is the child's literal name as `payload_format::xml`
/// addresses it; a prefixed entry (`ns:name`) also names the declared
/// local `name`, since declarations are namespace-free.
///
/// The two sides disagree on spelling: validation matches LOCAL names while
/// the projection keeps a child only under its LITERAL name. A kept child is
/// therefore guaranteed to survive projection only when the policy names it
/// unprefixed and the instance does too; `filter_group` relaxes requiredness
/// of every child for which that cannot be promised.
struct Kept<'a> {
    allowed: &'a BTreeSet<String>,
    /// Local part of every prefixed policy entry, built once so a lookup does
    /// not scan the whole policy for every child of every group.
    locals: HashSet<&'a str>,
}

impl<'a> Kept<'a> {
    fn new(allowed: &'a BTreeSet<String>) -> Kept<'a> {
        Kept {
            allowed,
            locals: allowed
                .iter()
                .filter_map(|a| a.rsplit_once(':').map(|(_, local)| local))
                .collect(),
        }
    }

    fn keeps(&self, name: &str) -> bool {
        self.allowed.contains(name) || self.locals.contains(name)
    }
}

/// The group restricted to allowed child elements, or `None` when nothing
/// is left. A `choice` that lost a whole alternative becomes optional: a
/// document that picked the dropped alternative projects to no element.
fn filter_group(g: &GroupDef, kept: &Kept, namespaced: bool) -> Option<GroupDef> {
    let mut items = Vec::new();
    let mut lost = false;
    for item in &g.items {
        match item {
            Item::Element(e) if kept.keeps(&e.name) => {
                let mut element = e.clone();
                if namespaced || !kept.allowed.contains(&e.name) {
                    element.min = 0;
                }
                items.push(Item::Element(element));
            }
            Item::Element(_) => lost = true,
            Item::Group(sub) => match filter_group(sub, kept, namespaced) {
                Some(f) => items.push(Item::Group(f)),
                None => lost = true,
            },
        }
    }
    if items.is_empty() {
        return None;
    }
    let min = if g.kind == GroupKind::Choice && lost {
        0
    } else {
        g.min
    };
    Some(GroupDef {
        kind: g.kind,
        min,
        max: g.max,
        items,
        prohibited: Vec::new(),
    })
}

fn project_complex(def: ComplexDef, kept: &Kept, namespaced: bool) -> ComplexDef {
    let content = match def.content {
        ComplexContent::Group(g) => {
            filter_group(&g, kept, namespaced).map_or(ComplexContent::Empty, ComplexContent::Group)
        }
        // The projection drops the root's own character data, so whatever
        // the text type demanded can no longer be promised.
        ComplexContent::Text(_) => ComplexContent::Text(SimpleBase::Builtin(Builtin::String)),
        other => other,
    };
    ComplexDef {
        content,
        attributes: def.attributes,
    }
}

fn named_in_use(t: &TypeUse, out: &mut Vec<String>) {
    match t {
        TypeUse::Named(n) => out.push(n.clone()),
        TypeUse::Builtin(_) => {}
        TypeUse::Simple(def) => named_in_base(&def.base, out),
        TypeUse::Complex(def) => named_in_complex(def, out),
    }
}

fn named_in_base(b: &SimpleBase, out: &mut Vec<String>) {
    if let SimpleBase::Named(n) = b {
        out.push(n.clone());
    }
}

fn named_in_group(g: &GroupDef, out: &mut Vec<String>) {
    for item in &g.items {
        match item {
            Item::Element(e) => named_in_use(&e.ty, out),
            Item::Group(sub) => named_in_group(sub, out),
        }
    }
}

fn named_in_complex(def: &ComplexDef, out: &mut Vec<String>) {
    for a in &def.attributes {
        named_in_use(&a.ty, out);
    }
    match &def.content {
        ComplexContent::Empty => {}
        ComplexContent::Text(b) => named_in_base(b, out),
        ComplexContent::Group(g) => named_in_group(g, out),
    }
}

/// Removes named types no longer reachable from a global element, so a
/// pattern or enumeration that only described a hidden field cannot leak.
fn prune_unreachable(doc: &mut Doc) {
    let mut pending = Vec::new();
    for e in &doc.elements {
        named_in_use(&e.ty, &mut pending);
    }
    let mut reachable = HashSet::new();
    while let Some(name) = pending.pop() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        if let Some((_, def)) = doc.simple_types.iter().find(|(n, _)| *n == name) {
            named_in_base(&def.base, &mut pending);
        }
        if let Some((_, def)) = doc.complex_types.iter().find(|(n, _)| *n == name) {
            named_in_complex(def, &mut pending);
        }
    }
    doc.simple_types.retain(|(n, _)| reachable.contains(n));
    doc.complex_types.retain(|(n, _)| reachable.contains(n));
}

fn derive_doc(doc: &Doc, allowed: &BTreeSet<String>) -> Doc {
    let kept = Kept::new(allowed);
    let mut out = doc.clone();
    for element in &mut out.elements {
        let def = match &element.ty {
            TypeUse::Named(n) => doc
                .complex_types
                .iter()
                .find(|(name, _)| name == n)
                .map(|(_, d)| d.clone()),
            TypeUse::Complex(d) => Some((**d).clone()),
            _ => None,
        };
        if let Some(def) = def {
            // A named type is inlined for the root only: other uses of it
            // (deeper in the tree) keep their full shape.
            element.ty = TypeUse::Complex(Box::new(project_complex(def, &kept, doc.namespaced)));
        } else {
            // A root of a simple type is all text, which the projection drops.
            element.ty = TypeUse::Builtin(Builtin::String);
        }
    }
    prune_unreachable(&mut out);
    out
}

// ---- rendering ----------------------------------------------------------

fn escape(s: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\t' | '\n' | '\r' if attribute => {
                let _ = write!(out, "&#{};", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

fn pad(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn type_attr(t: &TypeUse) -> Option<String> {
    match t {
        TypeUse::Builtin(b) => Some(format!("xs:{}", b.name())),
        TypeUse::Named(n) => Some(n.clone()),
        TypeUse::Simple(_) | TypeUse::Complex(_) => None,
    }
}

fn base_attr(b: &SimpleBase) -> String {
    match b {
        SimpleBase::Builtin(b) => format!("xs:{}", b.name()),
        SimpleBase::Named(n) => n.clone(),
    }
}

fn occurs_attrs(min: u32, max: Max) -> String {
    let mut s = String::new();
    if min != 1 {
        let _ = write!(s, " minOccurs=\"{min}\"");
    }
    match max {
        Max::Bounded(1) => {}
        Max::Bounded(m) => {
            let _ = write!(s, " maxOccurs=\"{m}\"");
        }
        Max::Unbounded => s.push_str(" maxOccurs=\"unbounded\""),
    }
    s
}

fn render_inline(out: &mut String, t: &TypeUse, depth: usize) {
    match t {
        TypeUse::Simple(def) => render_simple(out, def, None, depth),
        TypeUse::Complex(def) => render_complex(out, def, None, depth),
        TypeUse::Builtin(_) | TypeUse::Named(_) => {}
    }
}

fn render_element(out: &mut String, e: &ElementDef, depth: usize, global: bool) {
    pad(out, depth);
    let _ = write!(out, "<xs:element name=\"{}\"", escape(&e.name, true));
    if let Some(t) = type_attr(&e.ty) {
        let _ = write!(out, " type=\"{}\"", escape(&t, true));
    }
    if !global {
        out.push_str(&occurs_attrs(e.min, e.max));
    }
    if type_attr(&e.ty).is_some() {
        out.push_str("/>\n");
    } else {
        out.push_str(">\n");
        render_inline(out, &e.ty, depth + 1);
        pad(out, depth);
        out.push_str("</xs:element>\n");
    }
}

fn render_group(out: &mut String, g: &GroupDef, depth: usize) {
    let tag = match g.kind {
        GroupKind::Sequence => "sequence",
        GroupKind::Choice => "choice",
        GroupKind::All => "all",
    };
    pad(out, depth);
    let _ = writeln!(out, "<xs:{tag}{}>", occurs_attrs(g.min, g.max));
    for item in &g.items {
        match item {
            Item::Element(e) => render_element(out, e, depth + 1, false),
            Item::Group(sub) => render_group(out, sub, depth + 1),
        }
    }
    pad(out, depth);
    let _ = writeln!(out, "</xs:{tag}>");
}

fn render_attribute(out: &mut String, a: &AttrDef, depth: usize) {
    pad(out, depth);
    let _ = write!(out, "<xs:attribute name=\"{}\"", escape(&a.name, true));
    if let Some(t) = type_attr(&a.ty) {
        let _ = write!(out, " type=\"{}\"", escape(&t, true));
    }
    if a.required {
        out.push_str(" use=\"required\"");
    }
    if type_attr(&a.ty).is_some() {
        out.push_str("/>\n");
    } else {
        out.push_str(">\n");
        render_inline(out, &a.ty, depth + 1);
        pad(out, depth);
        out.push_str("</xs:attribute>\n");
    }
}

fn render_simple(out: &mut String, def: &SimpleDef, name: Option<&str>, depth: usize) {
    pad(out, depth);
    match name {
        Some(n) => {
            let _ = writeln!(out, "<xs:simpleType name=\"{}\">", escape(n, true));
        }
        None => out.push_str("<xs:simpleType>\n"),
    }
    pad(out, depth + 1);
    let _ = writeln!(
        out,
        "<xs:restriction base=\"{}\">",
        escape(&base_attr(&def.base), true)
    );
    let f = &def.facets;
    let mut facet = |tag: &str, value: &str| {
        pad(out, depth + 2);
        let _ = writeln!(out, "<xs:{tag} value=\"{}\"/>", escape(value, true));
    };
    if let Some(m) = f.min_length {
        facet("minLength", &m.to_string());
    }
    if let Some(m) = f.max_length {
        facet("maxLength", &m.to_string());
    }
    for p in &f.patterns {
        facet("pattern", p);
    }
    for e in &f.enumeration {
        facet("enumeration", e);
    }
    pad(out, depth + 1);
    out.push_str("</xs:restriction>\n");
    pad(out, depth);
    out.push_str("</xs:simpleType>\n");
}

fn render_complex(out: &mut String, def: &ComplexDef, name: Option<&str>, depth: usize) {
    pad(out, depth);
    match name {
        Some(n) => {
            let _ = writeln!(out, "<xs:complexType name=\"{}\">", escape(n, true));
        }
        None => out.push_str("<xs:complexType>\n"),
    }
    match &def.content {
        ComplexContent::Empty => {
            for a in &def.attributes {
                render_attribute(out, a, depth + 1);
            }
        }
        ComplexContent::Text(base) => {
            pad(out, depth + 1);
            out.push_str("<xs:simpleContent>\n");
            pad(out, depth + 2);
            let _ = writeln!(
                out,
                "<xs:extension base=\"{}\">",
                escape(&base_attr(base), true)
            );
            for a in &def.attributes {
                render_attribute(out, a, depth + 3);
            }
            pad(out, depth + 2);
            out.push_str("</xs:extension>\n");
            pad(out, depth + 1);
            out.push_str("</xs:simpleContent>\n");
        }
        ComplexContent::Group(g) => {
            render_group(out, g, depth + 1);
            for a in &def.attributes {
                render_attribute(out, a, depth + 1);
            }
        }
    }
    pad(out, depth);
    out.push_str("</xs:complexType>\n");
}

fn render(doc: &Doc) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(out, "<xs:schema xmlns:xs=\"{XSD_NS}\">");
    if let Some(d) = &doc.description {
        out.push_str("  <xs:annotation>\n");
        let _ = writeln!(
            out,
            "    <xs:documentation>{}</xs:documentation>",
            escape(d, false)
        );
        out.push_str("  </xs:annotation>\n");
    }
    for e in &doc.elements {
        render_element(&mut out, e, 1, true);
    }
    for (n, d) in &doc.simple_types {
        render_simple(&mut out, d, Some(n), 1);
    }
    for (n, d) in &doc.complex_types {
        render_complex(&mut out, d, Some(n), 1);
    }
    out.push_str("</xs:schema>\n");
    out
}

// =============================================================================
// check_compatibility
// =============================================================================

enum ContentView<'a> {
    Empty,
    Text(&'a SimpleType),
    Model(&'a Model),
}

fn view(c: &Compiled, ty: usize) -> (Option<&Attrs>, ContentView<'_>) {
    match &c.types[ty] {
        TypeDef::Simple(s) => (None, ContentView::Text(s)),
        TypeDef::Complex(ct) => (
            Some(&ct.attributes),
            match &ct.content {
                Content::Empty => ContentView::Empty,
                Content::Text(i) => ContentView::Text(c.simple(*i)),
                Content::Model(m) => ContentView::Model(m),
            },
        ),
    }
}

/// Decides L(sub) ⊆ L(sup) conservatively: `Ok` only when inclusion is
/// proven. `sup` is the schema that must accept, `sub` the one producing
/// the documents.
struct Inclusion<'a> {
    sup: &'a Compiled,
    sub: &'a Compiled,
    sup_label: &'static str,
    sub_label: &'static str,
    visited: HashSet<(usize, usize)>,
    pairs: usize,
    max_pairs: usize,
    /// Everything expensive any comparison does (NFA states, pattern and
    /// enumeration scans, attribute pairs), charged as it happens so the
    /// limit holds across nested comparisons.
    budget: Budget,
    /// Nesting of `types` calls; named complex types can chain far deeper
    /// than the inline nesting limit.
    depth: usize,
}

const MAX_COMPAT_DEPTH: usize = 64;

const TOO_COMPLEX: &str = "the schemas are too complex to compare; compatibility cannot be proven";

impl From<LimitExceeded> for String {
    fn from(_: LimitExceeded) -> String {
        TOO_COMPLEX.to_string()
    }
}

impl<'a> Inclusion<'a> {
    fn fail(&self, path: &[String], msg: &str) -> String {
        format!("/{}: {msg}", path.join("/"))
    }

    fn pair(&mut self) -> Result<(), String> {
        self.pairs += 1;
        if self.pairs > self.max_pairs {
            return Err(TOO_COMPLEX.to_string());
        }
        Ok(())
    }

    fn roots(&mut self) -> Result<(), String> {
        self.budget.charge(self.sub.roots.len() as u64)?;
        let mut names: Vec<&String> = self.sub.roots.keys().collect();
        names.sort();
        for name in names {
            let Some(&sup_el) = self.sup.roots.get(name) else {
                return Err(format!(
                    "document root '{name}' exists in the {} schema but not in the {} schema",
                    self.sub_label, self.sup_label
                ));
            };
            let sub_el = self.sub.roots[name];
            let mut path = vec![name.clone()];
            self.types(
                self.sup.elements[sup_el].ty,
                self.sub.elements[sub_el].ty,
                &mut path,
            )?;
        }
        Ok(())
    }

    fn types(
        &mut self,
        sup_ty: usize,
        sub_ty: usize,
        path: &mut Vec<String>,
    ) -> Result<(), String> {
        if !self.visited.insert((sup_ty, sub_ty)) {
            return Ok(());
        }
        if self.depth >= MAX_COMPAT_DEPTH {
            return Err(TOO_COMPLEX.to_string());
        }
        self.depth += 1;
        let result = self.types_unchecked(sup_ty, sub_ty, path);
        self.depth -= 1;
        result
    }

    fn types_unchecked(
        &mut self,
        sup_ty: usize,
        sub_ty: usize,
        path: &mut Vec<String>,
    ) -> Result<(), String> {
        self.pair()?;
        let (sup_attrs, sup_content) = view(self.sup, sup_ty);
        let (sub_attrs, sub_content) = view(self.sub, sub_ty);
        let sup_decls: &[AttrDecl] = sup_attrs.map_or(&[], |a| &a.decls);
        let sub_decls: &[AttrDecl] = sub_attrs.map_or(&[], |a| &a.decls);
        self.budget
            .charge(1 + (sup_decls.len() + sub_decls.len()) as u64)?;
        let (sup_c, sub_c) = (self.sup, self.sub);
        for b in sub_decls {
            let Some(a) = sup_attrs.and_then(|x| x.get(&b.name)) else {
                return Err(self.fail(
                    path,
                    &format!(
                        "attribute '{}' exists in the {} schema but not in the {} schema",
                        b.name, self.sub_label, self.sup_label
                    ),
                ));
            };
            self.simple(sup_c.simple(a.ty), sub_c.simple(b.ty))
                .map_err(|m| {
                    if m == TOO_COMPLEX {
                        m
                    } else {
                        self.fail(path, &format!("attribute '{}': {m}", b.name))
                    }
                })?;
            if a.required && !b.required {
                return Err(self.fail(
                    path,
                    &format!(
                        "attribute '{}' is required in the {} schema but optional in the {} schema",
                        b.name, self.sup_label, self.sub_label
                    ),
                ));
            }
        }
        for a in sup_decls.iter().filter(|a| a.required) {
            if sub_attrs.and_then(|x| x.get(&a.name)).is_none() {
                return Err(self.fail(
                    path,
                    &format!(
                        "attribute '{}' is required in the {} schema but absent from the {} schema",
                        a.name, self.sup_label, self.sub_label
                    ),
                ));
            }
        }
        match (sup_content, sub_content) {
            (ContentView::Empty, ContentView::Empty) => Ok(()),
            (ContentView::Text(a), ContentView::Text(b)) => self.simple(a, b).map_err(|m| {
                if m == TOO_COMPLEX {
                    m
                } else {
                    self.fail(path, &m)
                }
            }),
            (ContentView::Empty, ContentView::Model(m)) => {
                let no_children = match m {
                    Model::Nfa(n) => n.children.is_empty(),
                    Model::All(a) => a.children.is_empty(),
                };
                if no_children {
                    Ok(())
                } else {
                    Err(self.fail(
                        path,
                        &format!(
                            "the {} schema allows child elements where the {} schema allows none",
                            self.sub_label, self.sup_label
                        ),
                    ))
                }
            }
            (ContentView::Model(m), ContentView::Empty) => {
                let nullable = match m {
                    Model::Nfa(n) => n.nfa.accepts(&n.nfa.start_set),
                    Model::All(a) => a.accepts_empty(),
                };
                if nullable {
                    Ok(())
                } else {
                    Err(self.fail(
                        path,
                        &format!(
                            "the {} schema requires child elements that the {} schema may omit",
                            self.sup_label, self.sub_label
                        ),
                    ))
                }
            }
            (ContentView::Model(a), ContentView::Model(b)) => self.models(a, b, path),
            _ => Err(self.fail(
                path,
                "the content changed between text and child elements, or between empty and text",
            )),
        }
    }

    fn simple(&mut self, sup: &SimpleType, sub: &SimpleType) -> Result<(), String> {
        let patterns = |t: &SimpleType| t.steps.iter().map(|s| s.patterns.len()).sum::<usize>();
        self.budget
            .charge((1 + patterns(sup) + patterns(sub)) as u64)?;
        if !sub.builtin.widens_to(sup.builtin) {
            return Err(format!(
                "type xs:{} ({}) is not contained in xs:{} ({})",
                sub.builtin.name(),
                self.sub_label,
                sup.builtin.name(),
                self.sup_label
            ));
        }
        if sup.steps.is_empty() {
            return Ok(());
        }
        if let Some(members) = sub.effective_enumeration(&mut self.budget)? {
            // A member is the canonical value for numbers and booleans, but
            // the instance may spell it many ways (`01`, `+1`, ` 1 `, `1` for
            // true), and `sup.check` on the canonical form says nothing about
            // those spellings against a pattern or against a string type
            // (which sees the raw text). Provable are exact string literals,
            // and values compared as values: a non-string target without a
            // pattern.
            let provable = sub.builtin == Builtin::String
                || (sup.builtin != Builtin::String
                    && sup.steps.iter().all(|s| s.patterns.is_empty()));
            if !provable {
                return Err(
                    "an enumeration over a non-string type cannot be compared with a pattern or \
                     a string type; compatibility cannot be proven"
                        .to_string(),
                );
            }
            // `check` charges the scans of each member itself.
            for member in members {
                match sup.check(member, &mut self.budget) {
                    Ok(()) => {}
                    Err(CheckFailure::Violated(_)) => {
                        return Err(format!(
                            "enumerated value '{member}' of the {} schema is not accepted by \
                             the {} schema",
                            self.sub_label, self.sup_label
                        ));
                    }
                    Err(CheckFailure::TooComplex) => return Err(TOO_COMPLEX.to_string()),
                }
            }
            return Ok(());
        }
        if sub.builtin != sup.builtin {
            return Err(
                "restrictions cannot be compared across a type change; compatibility cannot be \
                 proven"
                    .to_string(),
            );
        }
        if sup.steps.iter().any(|s| !s.enumeration.is_empty()) {
            return Err(format!(
                "the {} schema restricts the value to an enumeration that the {} schema does not",
                self.sup_label, self.sub_label
            ));
        }
        let (sup_min, sup_max) = sup.effective_length();
        let (sub_min, sub_max) = sub.effective_length();
        if sub_min < sup_min || sup_max.is_some_and(|m| sub_max.is_none_or(|b| b > m)) {
            return Err(format!(
                "the length bounds of the {} schema are wider than those of the {} schema",
                self.sub_label, self.sup_label
            ));
        }
        let bytes = |t: &SimpleType| -> u64 {
            t.steps
                .iter()
                .flat_map(|s| s.patterns.iter())
                .map(|p| p.source.len() as u64)
                .sum()
        };
        // Sorting both sides and comparing each want against each have.
        self.budget.charge(
            (bytes(sup) + bytes(sub))
                .saturating_mul(8)
                .saturating_add((sup.steps.len() * sub.steps.len()) as u64),
        )?;
        let wants = sorted_sources(sup);
        let haves = sorted_sources(sub);
        for want in wants.iter().filter(|w| !w.is_empty()) {
            if !haves.iter().any(|have| have == want) {
                return Err(format!(
                    "the {} schema applies a pattern that the {} schema does not guarantee",
                    self.sup_label, self.sub_label
                ));
            }
        }
        Ok(())
    }

    fn models(&mut self, sup: &Model, sub: &Model, path: &mut Vec<String>) -> Result<(), String> {
        match (sup, sub) {
            (Model::All(a), Model::All(b)) => {
                for (i, child) in b.children.iter().enumerate() {
                    let Some(&j) = a.by_name.get(&child.name) else {
                        return Err(self.fail(
                            path,
                            &format!(
                                "child element '{}' exists in the {} schema but not in the {} \
                                 schema",
                                child.name, self.sub_label, self.sup_label
                            ),
                        ));
                    };
                    if a.required[j as usize] && !b.required[i] {
                        return Err(self.fail(
                            path,
                            &format!(
                                "child element '{}' is required in the {} schema but optional \
                                 in the {} schema",
                                child.name, self.sup_label, self.sub_label
                            ),
                        ));
                    }
                    path.push(child.name.clone());
                    self.types(
                        self.sup.elements[a.children[j as usize].element].ty,
                        self.sub.elements[child.element].ty,
                        path,
                    )?;
                    path.pop();
                }
                for (j, child) in a.children.iter().enumerate() {
                    if a.required[j] && !b.by_name.contains_key(&child.name) {
                        return Err(self.fail(
                            path,
                            &format!(
                                "child element '{}' is required in the {} schema but absent \
                                 from the {} schema",
                                child.name, self.sup_label, self.sub_label
                            ),
                        ));
                    }
                }
                if b.accepts_empty() && !a.accepts_empty() {
                    return Err(self.fail(
                        path,
                        &format!(
                            "the {} schema allows the group to be empty, the {} schema does not",
                            self.sub_label, self.sup_label
                        ),
                    ));
                }
                Ok(())
            }
            (Model::Nfa(a), Model::Nfa(b)) => self.nfas(a, b, path),
            _ => Err(self.fail(
                path,
                "the model group changed between xs:all and xs:sequence/xs:choice; compatibility \
                 cannot be proven",
            )),
        }
    }

    fn nfas(
        &mut self,
        sup: &NfaModel,
        sub: &NfaModel,
        path: &mut Vec<String>,
    ) -> Result<(), String> {
        self.budget
            .charge((sup.nfa.states.len() + sub.nfa.states.len()) as u64)?;
        let mut sup_marks = Marks::new(sup.nfa.states.len());
        let mut sub_marks = Marks::new(sub.nfa.states.len());
        let start = (sub.nfa.start_set.clone(), sup.nfa.start_set.clone());
        let mut seen: HashSet<(Vec<u32>, Vec<u32>)> = HashSet::new();
        seen.insert(start.clone());
        let mut queue = VecDeque::from([start]);
        let mut typed: HashSet<u32> = HashSet::new();
        while let Some((s1, s2)) = queue.pop_front() {
            self.pair()?;
            self.budget.charge((s1.len() + s2.len()) as u64)?;
            if sub.nfa.accepts(&s1) && !sup.nfa.accepts(&s2) {
                return Err(self.fail(
                    path,
                    &format!(
                        "the {} schema accepts a sequence of child elements that ends where the \
                         {} schema still requires {}",
                        self.sub_label,
                        self.sup_label,
                        required_names(sup, &s2)
                    ),
                ));
            }
            for label in sub.nfa.labels_from(&s1) {
                let child = &sub.children[label as usize];
                let Some(&sup_label) = sup.by_name.get(&child.name) else {
                    return Err(self.fail(
                        path,
                        &format!(
                            "child element '{}' exists in the {} schema but not in the {} schema",
                            child.name, self.sub_label, self.sup_label
                        ),
                    ));
                };
                let mut next_sub = sub.nfa.step(&s1, label, &mut sub_marks, &mut self.budget)?;
                let mut next_sup =
                    sup.nfa
                        .step(&s2, sup_label, &mut sup_marks, &mut self.budget)?;
                self.budget
                    .charge((next_sub.len() + next_sup.len()) as u64)?;
                next_sub.sort_unstable();
                next_sup.sort_unstable();
                if next_sup.is_empty() {
                    let msg = if sup.nfa.accepts(&s2) {
                        format!(
                            "child element '{}' may appear in the {} schema at a position where \
                             the {} schema does not allow it",
                            child.name, self.sub_label, self.sup_label
                        )
                    } else {
                        format!(
                            "the {} schema requires {} where the {} schema may have child \
                             element '{}'",
                            self.sup_label,
                            required_names(sup, &s2),
                            self.sub_label,
                            child.name
                        )
                    };
                    return Err(self.fail(path, &msg));
                }
                if typed.insert(label) {
                    path.push(child.name.clone());
                    self.types(
                        self.sup.elements[sup.children[sup_label as usize].element].ty,
                        self.sub.elements[child.element].ty,
                        path,
                    )?;
                    path.pop();
                }
                let pair = (next_sub, next_sup);
                if seen.insert(pair.clone()) {
                    queue.push_back(pair);
                }
            }
        }
        Ok(())
    }
}

/// Names of the child elements `sup` can take next from the state set `s2`,
/// worded as the stable phrase the dashboard parses:
/// `element 'a'` or `one of the elements 'a', 'b'`.
fn required_names(sup: &NfaModel, s2: &[u32]) -> String {
    let names: Vec<String> = sup
        .nfa
        .labels_from(s2)
        .into_iter()
        .map(|l| format!("'{}'", sup.children[l as usize].name))
        .collect();
    match names.as_slice() {
        [one] => format!("element {one}"),
        _ => format!("one of the elements {}", names.join(", ")),
    }
}

/// `Ok` iff every document valid under `sub` is proven valid under `sup`.
fn included(
    sup: &Compiled,
    sub: &Compiled,
    sup_label: &'static str,
    sub_label: &'static str,
) -> Result<(), String> {
    Inclusion {
        sup,
        sub,
        sup_label,
        sub_label,
        visited: HashSet::new(),
        pairs: 0,
        max_pairs: MAX_COMPAT_PAIRS,
        budget: Budget::new(MAX_COMPAT_WORK),
        depth: 0,
    }
    .roots()
}

// =============================================================================
// SchemaKindOps
// =============================================================================

pub struct XsdOps;

fn compile_text(schema_text: &str) -> Result<Compiled, SchemaError> {
    compile_doc(&parse_doc(schema_text)?)
}

impl SchemaKindOps for XsdOps {
    fn compile(&self, schema_text: &str) -> Result<CompiledSchema, SchemaError> {
        compile_text(schema_text).map(CompiledSchema::Xsd)
    }

    fn validate_metered(
        &self,
        compiled: &CompiledSchema,
        payload: &[u8],
        shared: &mut ValidationBudget,
    ) -> Result<(), SchemaError> {
        let CompiledSchema::Xsd(c) = compiled else {
            return Err(invalid("validate called with a non-xsd compiled schema"));
        };
        let mut budget = Budget::new(shared.allowance(MAX_VALIDATION_STEPS));
        let verdict = validate_with(c, payload, &mut budget);
        shared.spend(budget.used);
        verdict
    }

    fn derive_subschema(
        &self,
        schema_text: &str,
        allowed: &BTreeSet<String>,
    ) -> Result<String, SchemaError> {
        let derived = derive_doc(&parse_doc(schema_text)?, allowed);
        // The projection must itself be a registrable schema.
        compile_doc(&derived)?;
        Ok(render(&derived))
    }

    fn check_compatibility(
        &self,
        old_schema_text: &str,
        new_schema_text: &str,
        mode: Compatibility,
    ) -> Result<(), SchemaError> {
        if mode == Compatibility::None {
            return Ok(());
        }
        let old = compile_text(old_schema_text)?;
        let new = compile_text(new_schema_text)?;
        let backward = || {
            included(&new, &old, "new", "old").map_err(|d| {
                compat_failure(
                    d,
                    "backward: a document valid under the old schema may be invalid under the \
                     new one",
                )
            })
        };
        let forward = || {
            included(&old, &new, "old", "new").map_err(|d| {
                compat_failure(
                    d,
                    "forward: a document valid under the new schema may be invalid under the \
                     old one",
                )
            })
        };
        match mode {
            Compatibility::Backward => backward(),
            Compatibility::Forward => forward(),
            Compatibility::Full => backward().and_then(|()| forward()),
            Compatibility::None => Ok(()),
        }
    }
}

/// A comparison that ran out of budget proved nothing either way, so it is
/// not an incompatibility (the registry reports it under its own outcome).
fn compat_failure(detail: String, lead: &str) -> SchemaError {
    if detail == TOO_COMPLEX {
        SchemaError::LimitExceeded(detail)
    } else {
        SchemaError::Incompatible(format!("{lead} - {detail}"))
    }
}

pub(super) static XSD_OPS: XsdOps = XsdOps;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::payload_format::PayloadFormat;
    use crate::bus::schema_registry::fixtures;

    fn schema(body: &str) -> String {
        format!(r#"<xs:schema xmlns:xs="{XSD_NS}">{body}</xs:schema>"#)
    }

    fn ops_compile(text: &str) -> Result<CompiledSchema, SchemaError> {
        XSD_OPS.compile(text)
    }

    fn compiled(text: &str) -> CompiledSchema {
        ops_compile(text).unwrap_or_else(|e| panic!("{e}"))
    }

    fn violation_of(schema_text: &str, doc: &str) -> String {
        match XSD_OPS.validate(&compiled(schema_text), doc.as_bytes()) {
            Err(SchemaError::Violation(m)) => m,
            other => panic!("expected a violation for {doc}, got {other:?}"),
        }
    }

    fn accepts(schema_text: &str, doc: &str) -> bool {
        XSD_OPS
            .validate(&compiled(schema_text), doc.as_bytes())
            .is_ok()
    }

    fn rejected(body: &str) -> String {
        match ops_compile(&schema(body)) {
            Err(SchemaError::Invalid(m)) => m,
            other => panic!("expected the schema to be rejected, got {other:?}"),
        }
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    // ---- golden fixtures --------------------------------------------------

    #[test]
    fn golden_fixtures_validate_as_declared() {
        let compiled = compiled(&fixtures::read("xsd", "patient.xsd"));
        for case in fixtures::cases("xsd", "xml") {
            let got = XSD_OPS.validate(&compiled, &case.payload);
            match &case.expect {
                None => assert!(got.is_ok(), "{}: {got:?}", case.name),
                Some(expect) => assert!(
                    matches!(&got, Err(SchemaError::Violation(m)) if m.contains(expect.as_str())),
                    "{}: expected a violation containing {expect:?}, got {got:?}",
                    case.name
                ),
            }
        }
    }

    #[test]
    fn violations_never_echo_text_or_attribute_values() {
        let schema_text = fixtures::read("xsd", "patient.xsd");
        let doc = "<patient><mrn>SECRET-MRN</mrn></patient>";
        let msg = violation_of(&schema_text, doc);
        assert!(!msg.contains("SECRET"), "{msg}");
        let doc = r#"<patient><mrn>AB123456</mrn><name><family>A</family><given>B</given></name><birthDate>1980-01-01</birthDate><visit ward="LEAKED">x</visit></patient>"#;
        let msg = violation_of(&schema_text, doc);
        assert!(!msg.contains("LEAKED"), "{msg}");
        let msg = violation_of(
            &schema(
                r#"<xs:element name="a"><xs:simpleType><xs:restriction base="xs:string"><xs:pattern value="never-shown-[0-9]"/></xs:restriction></xs:simpleType></xs:element>"#,
            ),
            "<a>x</a>",
        );
        assert!(!msg.contains("never-shown"), "{msg}");
    }

    #[test]
    fn violations_never_echo_entity_references_or_parser_errors() {
        let s = schema_with_attribute();
        for doc in [
            "<a>&SECRET;</a>",
            r#"<a ward="&SECRET;">x</a>"#,
            r#"<a ward="x"c="SECRET">x</a>"#,
            "<a>x</SECRET>",
            "<a SECRET>x</a>",
            "<a>&#xSECRET;</a>",
        ] {
            let got = XSD_OPS.validate(&compiled(&s), doc.as_bytes());
            let Err(SchemaError::Violation(m)) = got else {
                panic!("{doc}: expected a violation, got {got:?}");
            };
            assert!(!m.contains("SECRET"), "{doc}: {m}");
        }
    }

    fn schema_with_attribute() -> String {
        schema(
            r#"<xs:element name="a"><xs:complexType><xs:simpleContent><xs:extension base="xs:string"><xs:attribute name="ward" type="xs:string"/></xs:extension></xs:simpleContent></xs:complexType></xs:element>"#,
        )
    }

    #[test]
    fn a_closing_parenthesis_cannot_end_the_anchoring_group() {
        let msg = rejected(
            r#"<xs:element name="a"><xs:simpleType><xs:restriction base="xs:string"><xs:pattern value="[0-9]{3})|(.*"/></xs:restriction></xs:simpleType></xs:element>"#,
        );
        assert!(msg.contains("unbalanced"), "{msg}");
        let msg = rejected(
            r#"<xs:element name="a"><xs:simpleType><xs:restriction base="xs:string"><xs:pattern value="(ab"/></xs:restriction></xs:simpleType></xs:element>"#,
        );
        assert!(msg.contains("pattern"), "{msg}");
        // Balanced groups still work and stay anchored.
        let s = schema(
            r#"<xs:element name="a"><xs:simpleType><xs:restriction base="xs:string"><xs:pattern value="(ab|cd)e"/></xs:restriction></xs:simpleType></xs:element>"#,
        );
        assert!(accepts(&s, "<a>cde</a>"));
        assert!(!accepts(&s, "<a>xcde</a>"));
    }

    #[test]
    fn a_prohibited_all_element_is_still_checked_at_compile_time() {
        let all = |gone: &str| {
            schema(&format!(
                r#"<xs:element name="r"><xs:complexType><xs:all>
                     <xs:element name="a" type="xs:string"/>
                     <xs:element name="gone" {gone} minOccurs="0" maxOccurs="0"/>
                   </xs:all></xs:complexType></xs:element>"#
            ))
        };
        let Err(SchemaError::Invalid(m)) = ops_compile(&all(r#"type="nope""#)) else {
            panic!("an unknown type of a prohibited element must be refused");
        };
        assert!(m.contains("unknown type 'nope'"), "{m}");
        let Err(SchemaError::Invalid(m)) = ops_compile(&all(r#"type="xs:float""#)) else {
            panic!("an unsupported built-in must be refused");
        };
        assert!(m.contains("xs:float"), "{m}");
        assert!(ops_compile(&all(r#"type="xs:string""#)).is_ok());
    }

    #[test]
    fn an_all_element_with_max_occurs_zero_never_matches() {
        let s = schema(
            r#"<xs:element name="r"><xs:complexType><xs:all>
                 <xs:element name="a" type="xs:string"/>
                 <xs:element name="gone" type="xs:string" minOccurs="0" maxOccurs="0"/>
               </xs:all></xs:complexType></xs:element>"#,
        );
        assert!(accepts(&s, "<r><a/></r>"));
        assert!(violation_of(&s, "<r><a/><gone/></r>").contains("not allowed"));
    }

    // ---- compile: rejected constructs --------------------------------------

    #[test]
    fn compile_rejects_unsupported_constructs_with_a_clear_message() {
        let cases: &[(&str, &str)] = &[
            (
                r#"<xs:import namespace="urn:x"/><xs:element name="a" type="xs:string"/>"#,
                "schema composition",
            ),
            (
                r#"<xs:include schemaLocation="x.xsd"/><xs:element name="a" type="xs:string"/>"#,
                "schema composition",
            ),
            (
                r#"<xs:group name="g"><xs:sequence/></xs:group><xs:element name="a" type="xs:string"/>"#,
                "named groups",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:sequence><xs:any/></xs:sequence></xs:complexType></xs:element>"#,
                "wildcards",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:anyAttribute/></xs:complexType></xs:element>"#,
                "wildcards",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:sequence><xs:element name="b" type="xs:string"/></xs:sequence></xs:complexType><xs:key name="k"><xs:selector xpath="b"/><xs:field xpath="."/></xs:key></xs:element>"#,
                "identity constraints",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:complexContent><xs:extension base="x"/></xs:complexContent></xs:complexType></xs:element>"#,
                "type derivation",
            ),
            (
                r#"<xs:element name="a"><xs:complexType mixed="true"><xs:sequence/></xs:complexType></xs:element>"#,
                "mixed content",
            ),
            (
                r#"<xs:element name="a" type="xs:long"/>"#,
                "xs:long is not supported",
            ),
            (
                r#"<xs:element name="a" type="xs:anyType"/>"#,
                "xs:anyType is not supported",
            ),
            (r#"<xs:element name="a"/>"#, "has no type"),
            (r#"<xs:element ref="a"/>"#, "ref="),
            (
                r#"<xs:element name="a" type="xs:string" nillable="true"/>"#,
                "attribute 'nillable'",
            ),
            (
                r#"<xs:element name="a" type="xs:string" default="x"/>"#,
                "attribute 'default'",
            ),
            (
                r#"<xs:element name="a" type="xs:string" substitutionGroup="b"/>"#,
                "attribute 'substitutionGroup'",
            ),
            (
                r#"<xs:attribute name="x" type="xs:string"/><xs:element name="a" type="xs:string"/>"#,
                "global attributes",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:attribute name="x" use="prohibited"/></xs:complexType></xs:element>"#,
                "use=\"prohibited\"",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:attribute name="x" fixed="1"/></xs:complexType></xs:element>"#,
                "attribute 'fixed'",
            ),
            (
                r#"<xs:simpleType name="t"><xs:restriction base="xs:string"><xs:totalDigits value="3"/></xs:restriction></xs:simpleType><xs:element name="a" type="t"/>"#,
                "facet xs:totalDigits",
            ),
            (
                r#"<xs:simpleType name="t"><xs:restriction base="xs:string"><xs:length value="3"/></xs:restriction></xs:simpleType><xs:element name="a" type="t"/>"#,
                "facet xs:length",
            ),
            (
                r#"<xs:simpleType name="t"><xs:union memberTypes="xs:int xs:string"/></xs:simpleType><xs:element name="a" type="t"/>"#,
                "list and union",
            ),
            (
                r#"<xs:simpleType name="t"><xs:restriction base="xs:int"><xs:minLength value="3"/></xs:restriction></xs:simpleType><xs:element name="a" type="t"/>"#,
                "apply to xs:string only",
            ),
            (
                r#"<xs:simpleType name="t"><xs:restriction base="xs:string"><xs:minLength value="5"/><xs:maxLength value="2"/></xs:restriction></xs:simpleType><xs:element name="a" type="t"/>"#,
                "greater than maxLength",
            ),
            (
                r#"<xs:simpleType name="t"><xs:restriction base="xs:int"><xs:enumeration value="x"/></xs:restriction></xs:simpleType><xs:element name="a" type="t"/>"#,
                "not a valid xs:int",
            ),
            (
                r#"<xs:simpleType name="a"><xs:restriction base="b"/></xs:simpleType><xs:simpleType name="b"><xs:restriction base="a"/></xs:simpleType><xs:element name="e" type="a"/>"#,
                "defined in terms of itself",
            ),
            (
                r#"<xs:element name="a" type="missing"/>"#,
                "unknown type 'missing'",
            ),
            (
                r#"<xs:complexType name="c"/><xs:simpleType name="t"><xs:restriction base="c"/></xs:simpleType><xs:element name="a" type="t"/>"#,
                "is a complex type",
            ),
            (
                r#"<xs:complexType name="c"/><xs:complexType name="c"/><xs:element name="a" type="c"/>"#,
                "declared twice",
            ),
            (
                r#"<xs:element name="a" type="xs:string"/><xs:element name="a" type="xs:string"/>"#,
                "declared twice",
            ),
            (r#"<xs:complexType name="c"/>"#, "no global element"),
            (
                r#"<xs:element name="a"><xs:complexType><xs:sequence><xs:element name="b" type="xs:string"/><xs:element name="b" type="xs:int"/></xs:sequence></xs:complexType></xs:element>"#,
                "more than once",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:sequence><xs:element name="b" type="xs:string" minOccurs="3" maxOccurs="2"/></xs:sequence></xs:complexType></xs:element>"#,
                "greater than maxOccurs",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:sequence><xs:element name="b" type="xs:string" maxOccurs="-1"/></xs:sequence></xs:complexType></xs:element>"#,
                "not a non-negative integer",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:sequence><xs:element name="b" type="xs:string" maxOccurs="100001"/></xs:sequence></xs:complexType></xs:element>"#,
                "exceeds the limit",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:choice/></xs:complexType></xs:element>"#,
                "at least one particle",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:all><xs:element name="b" type="xs:string" maxOccurs="2"/></xs:all></xs:complexType></xs:element>"#,
                "at most once",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:sequence><xs:all/></xs:sequence></xs:complexType></xs:element>"#,
                "cannot be nested",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:attribute name="x"/><xs:sequence/></xs:complexType></xs:element>"#,
                "before its attributes",
            ),
            (
                r#"<xs:element name="a"><xs:complexType><xs:attribute name="x"/><xs:attribute name="x"/></xs:complexType></xs:element>"#,
                "declared twice",
            ),
            (
                r#"<xs:element name="bad name" type="xs:string"/>"#,
                "not a valid name",
            ),
            (
                r#"<xs:element name="a" type="xs:string" form="qualified"/>"#,
                "attribute 'form'",
            ),
            (
                r#"<xs:element name="a" minOccurs="0" type="xs:string"/>"#,
                "must not carry",
            ),
            (
                r#"<xs:element name="a" type="other:t"/>"#,
                "namespace prefix that is not declared",
            ),
            (r#"<foo:bar/>"#, "not declared on xs:schema"),
            (
                r#"<xs:element name="a" type="xs:string"><xs:unique name="u"/></xs:element>"#,
                "identity constraints",
            ),
        ];
        for (body, needle) in cases {
            let msg = rejected(body);
            assert!(msg.contains(needle), "{body}: {msg}");
        }
    }

    #[test]
    fn compile_rejects_malformed_documents() {
        for (text, needle) in [
            ("not xml at all", "root element"),
            ("", "no root element"),
            ("<a/>", "must be xs:schema"),
            (
                &format!(
                    r#"<xs:schema xmlns:xs="{XSD_NS}" version="1" bogus="x"><xs:element name="a" type="xs:string"/></xs:schema>"#
                ),
                "attribute 'bogus'",
            ),
            (
                &format!(r#"<!DOCTYPE s><xs:schema xmlns:xs="{XSD_NS}"/>"#),
                "DOCTYPE",
            ),
            (
                &format!(
                    r#"<xs:schema xmlns:xs="{XSD_NS}"><xs:element xmlns:y="urn:y" name="a" type="xs:string"/></xs:schema>"#
                ),
                "only supported on xs:schema",
            ),
            (
                &format!(
                    r#"<xs:schema xmlns:xs="{XSD_NS}"><xs:element name="a" type="xs:string">text</xs:element></xs:schema>"#
                ),
                "must not contain text",
            ),
            (
                &format!(r#"<xs:schema xmlns:xs="{XSD_NS}"><other/></xs:schema>"#),
                "not in the XML Schema namespace",
            ),
            (
                &format!(
                    r#"<xs:schema xmlns="urn:other" xmlns:xs="{XSD_NS}"><element name="a"/></xs:schema>"#
                ),
                "not in the XML Schema namespace",
            ),
            (
                &format!(
                    r#"<xs:schema xmlns:xs="{XSD_NS}"><xs:element name="a" type="xs:string"></xs:schema>"#
                ),
                "not well-formed",
            ),
        ] {
            let err = XSD_OPS.compile(text).unwrap_err();
            assert!(
                matches!(&err, SchemaError::Invalid(m) if m.contains(needle)),
                "{text}: {err:?}"
            );
        }
    }

    #[test]
    fn compile_accepts_prefix_free_xsd_namespace_and_other_prefixes() {
        for text in [
            format!(r#"<schema xmlns="{XSD_NS}"><element name="a" type="string"/></schema>"#),
            format!(
                r#"<xsd:schema xmlns:xsd="{XSD_NS}"><xsd:element name="a" type="xsd:int"/></xsd:schema>"#
            ),
        ] {
            assert!(XSD_OPS.compile(&text).is_ok(), "{text}");
        }
    }

    // ---- compile: limits ----------------------------------------------------

    #[test]
    fn compile_enforces_size_depth_and_expansion_limits() {
        let huge = format!(
            "{}<!-- {} -->{}",
            r#"<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">"#,
            "x".repeat(MAX_SCHEMA_TEXT_BYTES),
            "</xs:schema>"
        );
        assert!(
            matches!(XSD_OPS.compile(&huge), Err(SchemaError::Invalid(m)) if m.contains("byte limit"))
        );

        let mut nested = String::new();
        for _ in 0..MAX_SCHEMA_DEPTH + 2 {
            nested.push_str("<xs:complexType><xs:sequence><xs:element name=\"e\">");
        }
        assert!(rejected(&nested).contains("nested deeper"));

        let many = "<xs:key/>".repeat(MAX_SCHEMA_NODES + 1);
        assert!(rejected(&many).contains("more than"));

        // 200 children x 100000 occurrences expand far past the state budget.
        let blowup: String = (0..200)
            .map(|i| {
                format!(
                    r#"<xs:element name="c{i}" type="xs:string" minOccurs="0" maxOccurs="100000"/>"#
                )
            })
            .collect();
        let body = format!(
            r#"<xs:element name="a"><xs:complexType><xs:sequence>{blowup}</xs:sequence></xs:complexType></xs:element>"#
        );
        assert!(rejected(&body).contains("too large"));

        // Nested counted groups multiply.
        let body = r#"<xs:element name="a"><xs:complexType><xs:sequence minOccurs="1000" maxOccurs="1000"><xs:sequence minOccurs="1000" maxOccurs="1000"><xs:element name="b" type="xs:string"/></xs:sequence></xs:sequence></xs:complexType></xs:element>"#;
        assert!(rejected(body).contains("too large"));
    }

    #[test]
    fn compile_bounds_pattern_cost() {
        let pattern_type = |p: &str| {
            format!(
                r#"<xs:simpleType name="t"><xs:restriction base="xs:string"><xs:pattern value="{p}"/></xs:restriction></xs:simpleType><xs:element name="a" type="t"/>"#
            )
        };
        // Compiled-program size blow-up is refused by the regex size limit.
        let msg = rejected(&pattern_type("((a{1000}){1000}){1000}"));
        assert!(msg.contains("not a valid or supported"), "{msg}");
        let msg = rejected(&pattern_type(&"a".repeat(MAX_PATTERN_CHARS + 1)));
        assert!(msg.contains("exceeds"), "{msg}");
        let many: String = (0..=MAX_PATTERNS)
            .map(|i| format!(r#"<xs:simpleType name="t{i}"><xs:restriction base="xs:string"><xs:pattern value="a"/></xs:restriction></xs:simpleType>"#))
            .collect();
        assert!(
            rejected(&format!(r#"{many}<xs:element name="a" type="t0"/>"#)).contains("more than")
        );
    }

    // ---- pattern dialect ----------------------------------------------------

    fn matches_pattern(pattern: &str, value: &str) -> bool {
        build_pattern(pattern)
            .expect(pattern)
            .is_match(value, &mut Budget::new(u64::MAX))
            .expect("unlimited budget")
    }

    #[test]
    fn patterns_are_anchored_and_use_xsd_semantics() {
        assert!(matches_pattern("[a-c]+", "abc"));
        assert!(
            !matches_pattern("[a-c]+", "abcd"),
            "implicitly anchored at the end"
        );
        assert!(
            !matches_pattern("[a-c]+", "xabc"),
            "implicitly anchored at the start"
        );
        // ^ and $ are ordinary characters in XSD.
        assert!(matches_pattern("^a$", "^a$"));
        assert!(!matches_pattern("^a$", "a"));
        assert!(matches_pattern("a|b", "b"));
        assert!(
            !matches_pattern("a|b", "ab"),
            "alternation stays inside the anchors"
        );
        // '.' excludes line breaks.
        assert!(matches_pattern("a.c", "abc"));
        assert!(!matches_pattern("a.c", "a\nc"));
        assert!(!matches_pattern("a.c", "a\rc"));
        assert!(matches_pattern(r"\d{3}", "123"));
        assert!(matches_pattern(r"a\sb", "a b"));
        assert!(matches_pattern(r"\p{Lu}\p{L}*", "Ábc"));
        assert!(matches_pattern("[^a-c]", "d"));
        assert!(matches_pattern(r"[a\-z]", "-"));
        assert!(matches_pattern("[&~]", "&"));
        // Set operators and nested classes of the regex crate stay literal.
        assert!(matches_pattern("[a&&b]", "&"));
        assert!(matches_pattern("[a&&b]", "a"));
        assert!(!matches_pattern("[a&&b]", "c"));
        assert!(matches_pattern("[a~~b]", "~"));
        assert!(matches_pattern(r"[\]a]", "]"));
        assert!(matches_pattern("[a-]", "-"));
        assert!(matches_pattern("[-a]", "-"));
        assert!(matches_pattern(r"(ab){2,3}", "ababab"));
    }

    #[test]
    fn patterns_reject_constructs_that_differ_from_xsd() {
        for (pattern, needle) in [
            (r"\w+", "not supported"),
            (r"\i", "not supported"),
            (r"\c", "not supported"),
            (r"\b", "not supported"),
            (r"(?i)a", "'(?'"),
            (r"a*?", "another quantifier"),
            (r"a+*", "another quantifier"),
            (r"[a-z-[aeiou]]", "subtraction"),
            (r"[[]", "must be escaped"),
            (r"[]a]", "']' inside a character class"),
            (r"[^]a]", "']' inside a character class"),
            (r"[](])|(.*[])]", "']' inside a character class"),
            (r"\p{IsBasicLatin}", "Unicode blocks"),
            (r"\p{L", "unterminated"),
            (r"[abc", "unterminated"),
            (r"a\", "lone backslash"),
            (r"[a--b]", "'--'"),
            (r"(a", "unterminated group"),
        ] {
            let err = build_pattern(pattern).unwrap_err();
            assert!(err.contains(needle), "{pattern}: {err}");
        }
    }

    // ---- lexical types ------------------------------------------------------

    #[test]
    fn builtin_lexical_forms() {
        let ok = |b: Builtin, v: &str| valid_lexical(b, v);
        for v in ["0", "-1", "+7", "007", "2147483647", "-2147483648"] {
            assert!(ok(Builtin::Int, v), "{v}");
        }
        for v in [
            "2147483648",
            "-2147483649",
            "",
            "1.0",
            "1e3",
            "abc",
            "--1",
            "99999999999999",
        ] {
            assert!(!ok(Builtin::Int, v), "{v}");
        }
        assert!(ok(Builtin::Integer, "123456789012345678901234567890"));
        assert!(!ok(Builtin::Integer, "1.5"));
        for v in ["1", "-1.5", ".5", "5.", "+0.0", "000.100"] {
            assert!(ok(Builtin::Decimal, v), "{v}");
        }
        for v in [".", "", "1e5", "1,5", "NaN", "- 1"] {
            assert!(!ok(Builtin::Decimal, v), "{v}");
        }
        for v in ["true", "false", "1", "0"] {
            assert!(ok(Builtin::Boolean, v));
        }
        for v in ["TRUE", "yes", "", "2"] {
            assert!(!ok(Builtin::Boolean, v));
        }
        for v in [
            "2026-09-02",
            "2024-02-29",
            "2026-09-02Z",
            "2026-09-02+01:00",
            "2026-09-02-05:30",
            "10000-01-01",
        ] {
            assert!(ok(Builtin::Date, v), "{v}");
        }
        for v in [
            "0000-01-01",
            "2026-13-01",
            "2026-02-30",
            "2025-02-29",
            "26-09-02",
            "2026-9-2",
            "2026-09-02+25:00",
            "2026-09-02+14:30",
            "2026-09-02T00:00:00",
            "",
        ] {
            assert!(!ok(Builtin::Date, v), "{v}");
        }
        for v in [
            "2026-09-02T08:30:00",
            "2026-09-02T08:30:00Z",
            "2026-09-02T08:30:00.123+02:00",
            "2026-09-02T23:59:59",
        ] {
            assert!(ok(Builtin::DateTime, v), "{v}");
        }
        for v in [
            "2026-09-02",
            "2026-09-02T24:00:00",
            "2026-09-02T08:60:00",
            "2026-09-02T08:30:60",
            "2026-09-02T08:30",
            "2026-09-02 08:30:00",
            "2026-09-02T08:30:00.",
            "2026-09-02T8:30:00",
        ] {
            assert!(!ok(Builtin::DateTime, v), "{v}");
        }
    }

    #[test]
    fn whitespace_is_collapsed_for_non_strings_and_preserved_for_strings() {
        let s = schema(
            r#"<xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="n" type="xs:int"/>
                 <xs:element name="t"><xs:simpleType><xs:restriction base="xs:string"><xs:maxLength value="3"/></xs:restriction></xs:simpleType></xs:element>
               </xs:sequence></xs:complexType></xs:element>"#,
        );
        assert!(accepts(&s, "<r><n>\n  42 \t</n><t>abc</t></r>"));
        assert!(violation_of(&s, "<r><n>42</n><t>ab  </t></r>").contains("longer than maxLength"));
        assert!(accepts(&s, "<r><n>42</n><t>ab</t></r>"));
        // Length counts characters, not bytes.
        assert!(!accepts(&s, "<r><n>1</n><t>żółć</t></r>"));
        assert!(accepts(&s, "<r><n>1</n><t>żół</t></r>"));
    }

    #[test]
    fn enumerations_compare_canonical_values() {
        let s = schema(
            r#"<xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="n"><xs:simpleType><xs:restriction base="xs:integer"><xs:enumeration value="1"/><xs:enumeration value="-2"/></xs:restriction></xs:simpleType></xs:element>
                 <xs:element name="d"><xs:simpleType><xs:restriction base="xs:decimal"><xs:enumeration value="1.5"/></xs:restriction></xs:simpleType></xs:element>
                 <xs:element name="b"><xs:simpleType><xs:restriction base="xs:boolean"><xs:enumeration value="true"/></xs:restriction></xs:simpleType></xs:element>
               </xs:sequence></xs:complexType></xs:element>"#,
        );
        assert!(accepts(&s, "<r><n>+001</n><d>01.500</d><b>1</b></r>"));
        assert!(accepts(&s, "<r><n>-0002</n><d>1.5</d><b>true</b></r>"));
        assert!(!accepts(&s, "<r><n>2</n><d>1.5</d><b>true</b></r>"));
        assert!(!accepts(&s, "<r><n>1</n><d>1.6</d><b>true</b></r>"));
        assert!(!accepts(&s, "<r><n>1</n><d>1.5</d><b>false</b></r>"));
    }

    #[test]
    fn stacked_restrictions_all_apply() {
        let s = schema(
            r#"<xs:simpleType name="base"><xs:restriction base="xs:string"><xs:maxLength value="5"/></xs:restriction></xs:simpleType>
               <xs:simpleType name="derived"><xs:restriction base="base"><xs:pattern value="[a-z]+"/></xs:restriction></xs:simpleType>
               <xs:element name="r" type="derived"/>"#,
        );
        assert!(accepts(&s, "<r>abc</r>"));
        assert!(violation_of(&s, "<r>abcdef</r>").contains("longer than maxLength"));
        assert!(violation_of(&s, "<r>AB</r>").contains("does not match the pattern"));
        // Alternatives inside one restriction.
        let s = schema(
            r#"<xs:element name="r"><xs:simpleType><xs:restriction base="xs:string"><xs:pattern value="a+"/><xs:pattern value="b+"/></xs:restriction></xs:simpleType></xs:element>"#,
        );
        assert!(accepts(&s, "<r>aaa</r>") && accepts(&s, "<r>bb</r>") && !accepts(&s, "<r>ab</r>"));
    }

    // ---- content models -----------------------------------------------------

    #[test]
    fn occurrence_bounds_choice_and_nesting() {
        let s = schema(
            r#"<xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="a" type="xs:string" minOccurs="2" maxOccurs="3"/>
                 <xs:choice minOccurs="0" maxOccurs="2">
                   <xs:element name="b" type="xs:string"/>
                   <xs:sequence><xs:element name="c" type="xs:string"/><xs:element name="d" type="xs:string"/></xs:sequence>
                 </xs:choice>
                 <xs:element name="e" type="xs:string" minOccurs="0" maxOccurs="unbounded"/>
               </xs:sequence></xs:complexType></xs:element>"#,
        );
        assert!(accepts(&s, "<r><a/><a/></r>"));
        assert!(accepts(&s, "<r><a/><a/><a/><b/><c/><d/><e/><e/><e/></r>"));
        assert!(!accepts(&s, "<r><a/></r>"), "minOccurs 2");
        assert!(!accepts(&s, "<r><a/><a/><a/><a/></r>"), "maxOccurs 3");
        assert!(
            !accepts(&s, "<r><a/><a/><b/><b/><b/></r>"),
            "choice repeats at most twice"
        );
        assert!(!accepts(&s, "<r><a/><a/><c/></r>"), "c needs d");
        assert!(accepts(&s, "<r><a/><a/><c/><d/><b/></r>"));
        assert!(
            !accepts(&s, "<r><a/><a/><e/><b/></r>"),
            "e closes the choice"
        );
        assert!(accepts(&s, "<r><a/><a/><e/><e/></r>"));
    }

    #[test]
    fn maxoccurs_zero_prohibits_and_empty_content_rejects_children() {
        let s = schema(
            r#"<xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="gone" type="xs:string" minOccurs="0" maxOccurs="0"/>
                 <xs:element name="kept" type="xs:string"/>
               </xs:sequence></xs:complexType></xs:element>
               <xs:element name="empty"><xs:complexType/></xs:element>"#,
        );
        assert!(accepts(&s, "<r><kept/></r>"));
        assert!(!accepts(&s, "<r><gone/><kept/></r>"));
        assert!(accepts(&s, "<empty/>") && accepts(&s, "<empty>  </empty>"));
        assert!(!accepts(&s, "<empty><x/></empty>"));
        assert!(!accepts(&s, "<empty>text</empty>"));
    }

    #[test]
    fn all_group_allows_any_order_once_each() {
        let s = schema(
            r#"<xs:element name="r"><xs:complexType><xs:all>
                 <xs:element name="a" type="xs:string"/>
                 <xs:element name="b" type="xs:string"/>
                 <xs:element name="c" type="xs:string" minOccurs="0"/>
               </xs:all></xs:complexType></xs:element>
               <xs:element name="opt"><xs:complexType><xs:all minOccurs="0">
                 <xs:element name="a" type="xs:string"/>
               </xs:all></xs:complexType></xs:element>"#,
        );
        assert!(accepts(&s, "<r><b/><a/></r>"));
        assert!(accepts(&s, "<r><c/><b/><a/></r>"));
        assert!(violation_of(&s, "<r><a/></r>").contains("required child element 'b' is missing"));
        assert!(violation_of(&s, "<r><a/><a/><b/></r>").contains("more than once"));
        assert!(violation_of(&s, "<r><a/><z/><b/></r>").contains("not allowed here"));
        assert!(violation_of(&s, "<r/>").contains("required child elements are missing"));
        assert!(accepts(&s, "<opt/>"));
        assert!(violation_of(&s, "<opt><a/><a/></opt>").contains("more than once"));
    }

    #[test]
    fn recursive_types_validate_to_the_document_depth() {
        let s = schema(
            r#"<xs:complexType name="node"><xs:sequence>
                 <xs:element name="label" type="xs:string"/>
                 <xs:element name="child" type="node" minOccurs="0" maxOccurs="unbounded"/>
               </xs:sequence></xs:complexType>
               <xs:element name="tree" type="node"/>"#,
        );
        assert!(accepts(&s, "<tree><label>a</label><child><label>b</label><child><label>c</label></child></child></tree>"));
        assert!(!accepts(
            &s,
            "<tree><label>a</label><child><child/></child></tree>"
        ));
        // Depth is bounded rather than recursing without limit.
        let deep = format!(
            "<tree>{}</tree>",
            "<label>x</label><child>".repeat(MAX_DOC_DEPTH) + &"</child>".repeat(MAX_DOC_DEPTH)
        );
        let got = XSD_OPS.validate(&compiled(&s), deep.as_bytes());
        assert!(
            matches!(&got, Err(SchemaError::LimitExceeded(m)) if m.contains("nested too deeply")),
            "{got:?}"
        );
    }

    #[test]
    fn a_violation_deep_in_the_document_carries_a_short_path() {
        let s = schema(
            r#"<xs:complexType name="node"><xs:sequence>
                 <xs:element name="dziecko" type="node" minOccurs="0"/>
               </xs:sequence></xs:complexType>
               <xs:element name="drzewo" type="node"/>"#,
        );
        let depth = 100;
        let deep = format!(
            "<drzewo>{}<inne/>{}</drzewo>",
            "<dziecko>".repeat(depth),
            "</dziecko>".repeat(depth)
        );
        let m = violation_of(&s, &deep);
        assert!(
            m.starts_with("/drzewo/dziecko/…/dziecko/inne: element is not allowed here"),
            "{m}"
        );
        assert!(m.chars().count() < MAX_PATH_CHARS + 64, "{m}");
        // A shallow path is reported in full.
        let m = violation_of(&s, "<drzewo><dziecko><inne/></dziecko></drzewo>");
        assert!(m.starts_with("/drzewo/dziecko/inne: "), "{m}");
        // Names are bounded too, so a few long segments cannot widen the text.
        let (n1, n2, n3) = ("a".repeat(100), "b".repeat(100), "c".repeat(100));
        let wide = schema(&format!(
            r#"<xs:complexType name="t1"><xs:sequence><xs:element name="{n2}" type="t2"/></xs:sequence></xs:complexType>
               <xs:complexType name="t2"><xs:sequence><xs:element name="{n3}" type="t3"/></xs:sequence></xs:complexType>
               <xs:complexType name="t3"/>
               <xs:element name="{n1}" type="t1"/>"#
        ));
        let m = violation_of(
            &wide,
            &format!("<{n1}><{n2}><{n3}><x/></{n3}></{n2}></{n1}>"),
        );
        assert!(
            m.starts_with("/aaaa") && m.ends_with("…: element is not allowed here"),
            "{m}"
        );
        assert!(m.chars().count() < MAX_PATH_CHARS + 64, "{}", m.len());
    }

    #[test]
    fn hostile_documents_fail_closed_within_the_work_budget() {
        // 100k alternatives of a wide choice would be quadratic without a budget.
        let alternatives: String = (0..2000)
            .map(|i| format!(r#"<xs:element name="c{i}" type="xs:string"/>"#))
            .collect();
        let s = schema(&format!(
            r#"<xs:element name="r"><xs:complexType><xs:choice minOccurs="0" maxOccurs="unbounded">{alternatives}</xs:choice></xs:complexType></xs:element>"#
        ));
        let body: String = (0..2000).map(|i| format!("<c{i}/>")).collect();
        assert!(accepts(&s, &format!("<r>{body}</r>")));
        let big = format!("<r>{}</r>", "<c1999/>".repeat(30_000));
        let got = XSD_OPS.validate(&compiled(&s), big.as_bytes());
        assert!(
            matches!(&got, Err(SchemaError::LimitExceeded(m)) if m.contains("work budget")),
            "{got:?}"
        );
    }

    #[test]
    fn character_data_entities_and_cdata() {
        let s = schema(r#"<xs:element name="r" type="xs:string"/>"#);
        assert!(accepts(&s, "<r>a &amp; b &lt; &#65; &#x42;</r>"));
        assert!(accepts(&s, "<r><![CDATA[<raw>]]></r>"));
        assert!(violation_of(&s, "<r>&nope;</r>").contains("not supported"));
        assert!(violation_of(&s, "<r>x</r>junk").contains("outside the root"));
        assert!(accepts(
            &s,
            "<?xml version=\"1.0\"?>\n<!-- c -->\n<r/>\n<!-- end -->\n"
        ));
        assert!(violation_of(&s, "").contains("no root element"));
        assert!(
            violation_of(&s, "<r>").contains("open element")
                || violation_of(&s, "<r>").contains("not well-formed")
        );
        let invalid_utf8 = b"<r>\xff</r>";
        assert!(matches!(
            XSD_OPS.validate(&compiled(&s), invalid_utf8),
            Err(SchemaError::Violation(_))
        ));
    }

    #[test]
    fn simple_content_requires_attributes_and_checks_text() {
        let s = schema(
            r#"<xs:element name="price"><xs:complexType><xs:simpleContent><xs:extension base="xs:decimal">
                 <xs:attribute name="currency" use="required"><xs:simpleType><xs:restriction base="xs:string"><xs:enumeration value="EUR"/><xs:enumeration value="PLN"/></xs:restriction></xs:simpleType></xs:attribute>
                 <xs:attribute name="note"/>
               </xs:extension></xs:simpleContent></xs:complexType></xs:element>"#,
        );
        assert!(accepts(
            &s,
            r#"<price currency="EUR" note="anything">12.50</price>"#
        ));
        assert!(violation_of(&s, r#"<price currency="USD">1</price>"#).contains("enumerated"));
        assert!(violation_of(&s, "<price>1</price>").contains("required attribute 'currency'"));
        assert!(violation_of(&s, r#"<price currency="EUR">abc</price>"#).contains("xs:decimal"));
        assert!(violation_of(&s, r#"<price currency="EUR"><x/></price>"#).contains("not allowed"));
    }

    #[test]
    fn a_simple_typed_element_takes_no_attributes() {
        let s = schema(r#"<xs:element name="r" type="xs:int"/>"#);
        assert!(accepts(&s, "<r xmlns=\"urn:x\" xmlns:p=\"urn:p\">1</r>"));
        assert!(violation_of(&s, r#"<r a="1">1</r>"#).contains("attribute 'a' is not declared"));
    }

    #[test]
    fn multiple_global_elements_are_all_valid_roots() {
        let s = schema(
            r#"<xs:element name="a" type="xs:int"/><xs:element name="b" type="xs:boolean"/>"#,
        );
        assert!(accepts(&s, "<a>1</a>") && accepts(&s, "<b>true</b>"));
        assert!(!accepts(&s, "<a>true</a>") && !accepts(&s, "<c/>"));
    }

    // ---- derive_subschema ---------------------------------------------------

    #[test]
    fn derive_matches_golden_and_is_stable() {
        let source = fixtures::read("xsd", "patient.xsd");
        let allowed = set(&["mrn", "birthDate", "phone", "visit"]);
        let derived = XSD_OPS.derive_subschema(&source, &allowed).unwrap();
        assert_eq!(derived, fixtures::read("xsd", "patient.derived.xsd"));
        // Same inputs, same bytes; the derived schema is registrable and a
        // fixed point of the same projection.
        assert_eq!(
            derived,
            XSD_OPS.derive_subschema(&source, &allowed).unwrap()
        );
        assert!(XSD_OPS.compile(&derived).is_ok());
        assert_eq!(
            derived,
            XSD_OPS.derive_subschema(&derived, &allowed).unwrap()
        );
    }

    #[test]
    fn documents_projected_by_the_xml_field_policy_validate_against_the_derived_schema() {
        let source = fixtures::read("xsd", "patient.xsd");
        let original = compiled(&source);
        let doc = fixtures::cases("xsd", "xml")
            .into_iter()
            .find(|c| c.name == "valid-full")
            .unwrap()
            .payload;
        for allowed in [
            set(&["mrn", "birthDate", "phone", "visit"]),
            set(&["mrn", "name", "birthDate"]),
            set(&["email"]),
            set(&[]),
            set(&["mrn", "name", "birthDate", "sex", "phone", "email", "visit"]),
        ] {
            let derived = XSD_OPS.derive_subschema(&source, &allowed).unwrap();
            let projected = PayloadFormat::Xml.codec().project(&doc, &allowed).unwrap();
            let got = XSD_OPS.validate(&compiled(&derived), &projected);
            assert!(
                got.is_ok(),
                "{allowed:?}: {got:?}\n{}",
                String::from_utf8_lossy(&projected)
            );
            if !["mrn", "name", "birthDate"]
                .iter()
                .all(|f| allowed.contains(*f))
            {
                // The unprojected schema does reject what the policy removed.
                assert!(
                    XSD_OPS.validate(&original, &projected).is_err(),
                    "{allowed:?}"
                );
            }
        }
    }

    #[test]
    fn derive_makes_a_choice_optional_when_an_alternative_disappears() {
        let s = schema(
            r#"<xs:element name="r"><xs:complexType><xs:choice>
                 <xs:element name="a" type="xs:string"/>
                 <xs:element name="b" type="xs:string"/>
               </xs:choice></xs:complexType></xs:element>"#,
        );
        let derived = XSD_OPS.derive_subschema(&s, &set(&["a"])).unwrap();
        let c = compiled(&derived);
        assert!(XSD_OPS.validate(&c, b"<r><a/></r>").is_ok());
        // A document that picked `b` projects to an empty root.
        assert!(XSD_OPS.validate(&c, b"<r/>").is_ok());
        assert!(XSD_OPS.validate(&c, b"<r><b/></r>").is_err());
        // With both alternatives allowed the choice stays required.
        let full = XSD_OPS.derive_subschema(&s, &set(&["a", "b"])).unwrap();
        assert!(XSD_OPS.validate(&compiled(&full), b"<r/>").is_err());
    }

    #[test]
    fn derive_keeps_other_uses_of_a_named_type_and_prunes_hidden_declarations() {
        let s = schema(
            r#"<xs:simpleType name="Secret"><xs:restriction base="xs:string"><xs:pattern value="[0-9]{11}"/></xs:restriction></xs:simpleType>
               <xs:complexType name="Box"><xs:sequence>
                 <xs:element name="ssn" type="Secret"/>
                 <xs:element name="ok" type="xs:string"/>
                 <xs:element name="inner" type="Box" minOccurs="0"/>
               </xs:sequence></xs:complexType>
               <xs:element name="root" type="Box"/>"#,
        );
        let derived = XSD_OPS
            .derive_subschema(&s, &set(&["ok", "inner"]))
            .unwrap();
        // The root lost `ssn`, nested boxes (the named type) still need it ...
        let c = compiled(&derived);
        assert!(XSD_OPS.validate(&c, b"<root><ok/></root>").is_ok());
        assert!(XSD_OPS
            .validate(
                &c,
                b"<root><ok/><inner><ssn>12345678901</ssn><ok/></inner></root>"
            )
            .is_ok());
        assert!(XSD_OPS
            .validate(&c, b"<root><ssn>12345678901</ssn><ok/></root>")
            .is_err());
        // ... but when nothing reaches the type any more, it is dropped, and
        // with it the pattern that only described the hidden field.
        let derived = XSD_OPS.derive_subschema(&s, &set(&["ok"])).unwrap();
        assert!(
            !derived.contains("Secret") && !derived.contains("[0-9]{11}"),
            "{derived}"
        );
        assert!(!derived.contains("Box"), "{derived}");
    }

    #[test]
    fn derive_accepts_a_prefixed_policy_entry_for_a_declared_local_name() {
        let s = schema(
            r#"<xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="a" type="xs:string"/><xs:element name="b" type="xs:string"/>
               </xs:sequence></xs:complexType></xs:element>"#,
        );
        let derived = XSD_OPS.derive_subschema(&s, &set(&["p:a"])).unwrap();
        assert!(XSD_OPS
            .validate(&compiled(&derived), b"<r><a/></r>")
            .is_ok());
        assert!(XSD_OPS
            .validate(&compiled(&derived), b"<r><b/></r>")
            .is_err());
    }

    #[test]
    fn derive_preserves_root_attributes_and_description_and_round_trips_text() {
        let source = fixtures::read("xsd", "patient.xsd");
        let derived = XSD_OPS.derive_subschema(&source, &set(&["mrn"])).unwrap();
        assert!(derived.contains("Patient registration message used by the admissions desk."));
        assert!(derived.contains(r#"<xs:attribute name="active" type="xs:boolean"/>"#));
        // parse(render(doc)) is the identity, including escaping.
        let mut doc = parse_doc(&source).unwrap();
        // Rendering is namespace-free by design, so the flag is not carried.
        doc.namespaced = false;
        assert_eq!(parse_doc(&render(&doc)).unwrap(), doc);
        let tricky = schema(
            r#"<xs:annotation><xs:documentation>a &lt; b &amp; "c"</xs:documentation></xs:annotation>
               <xs:element name="r"><xs:simpleType><xs:restriction base="xs:string">
                 <xs:enumeration value="a&amp;b"/><xs:enumeration value="x&#9;y"/><xs:enumeration value="&lt;&gt;&quot;"/>
               </xs:restriction></xs:simpleType></xs:element>"#,
        );
        let doc = parse_doc(&tricky).unwrap();
        assert_eq!(parse_doc(&render(&doc)).unwrap(), doc);
    }

    // ---- check_compatibility --------------------------------------------------

    fn element_with(content: &str) -> String {
        schema(&format!(
            r#"<xs:element name="r"><xs:complexType>{content}</xs:complexType></xs:element>"#
        ))
    }

    fn seq(items: &str) -> String {
        element_with(&format!("<xs:sequence>{items}</xs:sequence>"))
    }

    fn el(name: &str, ty: &str, occurs: &str) -> String {
        format!(r#"<xs:element name="{name}" type="{ty}" {occurs}/>"#)
    }

    fn restricted(base: &str, facets: &str) -> String {
        schema(&format!(
            r#"<xs:element name="r"><xs:simpleType><xs:restriction base="{base}">{facets}</xs:restriction></xs:simpleType></xs:element>"#
        ))
    }

    #[test]
    fn compatibility_matrix() {
        let a = el("a", "xs:string", "");
        let b = el("b", "xs:string", "");
        let b_opt = el("b", "xs:string", r#"minOccurs="0""#);
        // (label, old, new, backward ok, forward ok)
        let cases: Vec<(&str, String, String, bool, bool)> = vec![
            ("identical", seq(&a), seq(&a), true, true),
            (
                "optional element added",
                seq(&a),
                seq(&format!("{a}{b_opt}")),
                true,
                false,
            ),
            (
                "required element added",
                seq(&a),
                seq(&format!("{a}{b}")),
                false,
                false,
            ),
            (
                "optional element removed",
                seq(&format!("{a}{b_opt}")),
                seq(&a),
                false,
                true,
            ),
            (
                "required element removed",
                seq(&format!("{a}{b}")),
                seq(&a),
                false,
                false,
            ),
            (
                "sequence reordered",
                seq(&format!("{a}{b}")),
                seq(&format!("{b}{a}")),
                false,
                false,
            ),
            (
                "maxOccurs widened",
                seq(&el("a", "xs:string", "")),
                seq(&el("a", "xs:string", r#"maxOccurs="unbounded""#)),
                true,
                false,
            ),
            (
                "minOccurs raised",
                seq(&el("a", "xs:string", r#"minOccurs="0""#)),
                seq(&el("a", "xs:string", "")),
                false,
                true,
            ),
            (
                "count range shifted",
                seq(&el("a", "xs:string", r#"minOccurs="1" maxOccurs="3""#)),
                seq(&el("a", "xs:string", r#"minOccurs="2" maxOccurs="4""#)),
                false,
                false,
            ),
            (
                "int widened to decimal",
                seq(&el("a", "xs:int", "")),
                seq(&el("a", "xs:decimal", "")),
                true,
                false,
            ),
            (
                "integer widened to string",
                seq(&el("a", "xs:integer", "")),
                seq(&el("a", "xs:string", "")),
                true,
                false,
            ),
            (
                "string narrowed to date",
                seq(&el("a", "xs:string", "")),
                seq(&el("a", "xs:date", "")),
                false,
                true,
            ),
            (
                "unrelated types",
                seq(&el("a", "xs:date", "")),
                seq(&el("a", "xs:dateTime", "")),
                false,
                false,
            ),
            (
                "choice alternative added",
                element_with(&format!("<xs:choice>{a}</xs:choice>")),
                element_with(&format!("<xs:choice>{a}{b}</xs:choice>")),
                true,
                false,
            ),
            (
                "sequence to choice",
                seq(&format!("{a}{b_opt}")),
                element_with(&format!("<xs:choice minOccurs=\"0\">{a}{b}</xs:choice>")),
                false,
                false,
            ),
            (
                "root added",
                schema(&el("r", "xs:string", "")),
                schema(&format!(
                    "{}{}",
                    el("r", "xs:string", ""),
                    el("s", "xs:string", "")
                )),
                true,
                false,
            ),
            (
                "root renamed",
                schema(&el("r", "xs:string", "")),
                schema(&el("q", "xs:string", "")),
                false,
                false,
            ),
            (
                "simple to complex",
                schema(&el("r", "xs:string", "")),
                seq(&a),
                false,
                false,
            ),
            (
                "empty to optional children",
                element_with(""),
                seq(&b_opt),
                true,
                false,
            ),
            (
                "empty to required children",
                element_with(""),
                seq(&b),
                false,
                false,
            ),
            (
                "all required relaxed",
                element_with(&format!("<xs:all>{a}{b}</xs:all>")),
                element_with(&format!("<xs:all>{a}{b_opt}</xs:all>")),
                true,
                false,
            ),
            (
                "all to sequence",
                element_with(&format!("<xs:all>{a}{b}</xs:all>")),
                seq(&format!("{a}{b}")),
                false,
                false,
            ),
        ];
        for (label, old, new, backward, forward) in &cases {
            for (mode, expected) in [
                (Compatibility::Backward, *backward),
                (Compatibility::Forward, *forward),
                (Compatibility::Full, *backward && *forward),
            ] {
                let got = XSD_OPS.check_compatibility(old, new, mode);
                assert_eq!(got.is_ok(), expected, "{label} under {mode:?}: {got:?}");
                if let Err(e) = got {
                    assert!(matches!(e, SchemaError::Incompatible(_)), "{label}: {e:?}");
                }
            }
            assert!(XSD_OPS
                .check_compatibility(old, new, Compatibility::None)
                .is_ok());
        }
    }

    #[test]
    fn compatibility_of_simple_type_restrictions() {
        let string = "xs:string";
        let enum_of = |vals: &[&str]| {
            vals.iter()
                .map(|v| format!(r#"<xs:enumeration value="{v}"/>"#))
                .collect::<String>()
        };
        let max_len = |n: u32| format!(r#"<xs:maxLength value="{n}"/>"#);
        let min_len = |n: u32| format!(r#"<xs:minLength value="{n}"/>"#);
        let pattern = |p: &str| format!(r#"<xs:pattern value="{p}"/>"#);
        // (label, old, new, backward ok, forward ok)
        let cases: Vec<(&str, String, String, bool, bool)> = vec![
            (
                "maxLength widened",
                restricted(string, &max_len(10)),
                restricted(string, &max_len(20)),
                true,
                false,
            ),
            (
                "maxLength removed",
                restricted(string, &max_len(10)),
                restricted(string, ""),
                true,
                false,
            ),
            (
                "maxLength added",
                restricted(string, ""),
                restricted(string, &max_len(10)),
                false,
                true,
            ),
            (
                "minLength raised",
                restricted(string, &min_len(1)),
                restricted(string, &min_len(2)),
                false,
                true,
            ),
            (
                "enumeration value added",
                restricted(string, &enum_of(&["a", "b"])),
                restricted(string, &enum_of(&["a", "b", "c"])),
                true,
                false,
            ),
            (
                "enumeration value removed",
                restricted(string, &enum_of(&["a", "b", "c"])),
                restricted(string, &enum_of(&["a", "b"])),
                false,
                true,
            ),
            (
                "enumeration dropped",
                restricted(string, &enum_of(&["a"])),
                restricted(string, ""),
                true,
                false,
            ),
            (
                "enumeration reordered",
                restricted(string, &enum_of(&["a", "b"])),
                restricted(string, &enum_of(&["b", "a"])),
                true,
                true,
            ),
            (
                "pattern identical",
                restricted(string, &pattern("[a-z]+")),
                restricted(string, &pattern("[a-z]+")),
                true,
                true,
            ),
            (
                "pattern changed (unprovable)",
                restricted(string, &pattern("[a-z]+")),
                restricted(string, &pattern("[a-z]*")),
                false,
                false,
            ),
            (
                "pattern dropped",
                restricted(string, &pattern("[a-z]+")),
                restricted(string, ""),
                true,
                false,
            ),
            (
                "enumeration satisfies the pattern",
                restricted(string, &pattern("[a-z]+")),
                restricted(string, &enum_of(&["abc", "de"])),
                false,
                true,
            ),
            (
                "int enumeration inside decimal range",
                restricted("xs:decimal", &enum_of(&["1", "2"])),
                restricted("xs:int", &enum_of(&["1"])),
                false,
                true,
            ),
            // The instance may spell the value `01`, `+1` or ` 1 `, which a
            // pattern or a string type judges on its raw text.
            (
                "int enumeration against an int pattern",
                restricted("xs:int", &pattern("[0-9]")),
                restricted("xs:int", &enum_of(&["1"])),
                false,
                false,
            ),
            (
                "int enumeration against a string enumeration",
                restricted(string, &enum_of(&["1"])),
                restricted("xs:int", &enum_of(&["1"])),
                false,
                false,
            ),
            (
                "boolean enumeration against a string enumeration",
                restricted(string, &enum_of(&["true"])),
                restricted("xs:boolean", &enum_of(&["true"])),
                false,
                false,
            ),
        ];
        for (label, old, new, backward, forward) in &cases {
            for (mode, expected) in [
                (Compatibility::Backward, *backward),
                (Compatibility::Forward, *forward),
            ] {
                let got = XSD_OPS.check_compatibility(old, new, mode);
                assert_eq!(got.is_ok(), expected, "{label} under {mode:?}: {got:?}");
            }
        }
    }

    #[test]
    fn compatibility_of_attributes() {
        let with_attrs = |attrs: &str| {
            element_with(&format!(
                r#"<xs:sequence>{}</xs:sequence>{attrs}"#,
                el("a", "xs:string", "")
            ))
        };
        let opt = r#"<xs:attribute name="x" type="xs:string"/>"#;
        let req = r#"<xs:attribute name="x" type="xs:string" use="required"/>"#;
        let req_int = r#"<xs:attribute name="x" type="xs:int" use="required"/>"#;
        let req_dec = r#"<xs:attribute name="x" type="xs:decimal" use="required"/>"#;
        let cases = [
            ("optional attribute added", "", opt, true, false),
            ("required attribute added", "", req, false, false),
            ("optional attribute removed", opt, "", false, true),
            ("required attribute removed", req, "", false, false),
            ("attribute became required", opt, req, false, true),
            ("attribute became optional", req, opt, true, false),
            ("attribute type widened", req_int, req_dec, true, false),
        ];
        for (label, old, new, backward, forward) in cases {
            let (old, new) = (with_attrs(old), with_attrs(new));
            assert_eq!(
                XSD_OPS
                    .check_compatibility(&old, &new, Compatibility::Backward)
                    .is_ok(),
                backward,
                "{label} backward"
            );
            assert_eq!(
                XSD_OPS
                    .check_compatibility(&old, &new, Compatibility::Forward)
                    .is_ok(),
                forward,
                "{label} forward"
            );
        }
    }

    #[test]
    fn compatibility_handles_recursive_types_and_ignores_documentation() {
        let recursive = |doc: &str| {
            schema(&format!(
                r#"{doc}<xs:complexType name="node"><xs:sequence>
                     <xs:element name="child" type="node" minOccurs="0"/>
                   </xs:sequence></xs:complexType><xs:element name="tree" type="node"/>"#
            ))
        };
        let plain = recursive("");
        let documented = recursive(
            "<xs:annotation><xs:documentation>changed text</xs:documentation></xs:annotation>",
        );
        assert!(XSD_OPS
            .check_compatibility(&plain, &documented, Compatibility::Full)
            .is_ok());
        let wider = plain.replace("minOccurs=\"0\"", "minOccurs=\"0\" maxOccurs=\"2\"");
        assert!(XSD_OPS
            .check_compatibility(&plain, &wider, Compatibility::Backward)
            .is_ok());
        assert!(XSD_OPS
            .check_compatibility(&plain, &wider, Compatibility::Forward)
            .is_err());
    }

    #[test]
    fn compatibility_messages_name_the_direction_and_the_path() {
        let old = seq(&el("a", "xs:string", ""));
        let new = seq(&format!(
            "{}{}",
            el("a", "xs:string", ""),
            el("b", "xs:string", "")
        ));
        let Err(SchemaError::Incompatible(msg)) =
            XSD_OPS.check_compatibility(&old, &new, Compatibility::Backward)
        else {
            panic!("expected Incompatible");
        };
        assert!(msg.starts_with("backward:") && msg.contains("/r"), "{msg}");
        let Err(SchemaError::Incompatible(msg)) =
            XSD_OPS.check_compatibility(&new, &old, Compatibility::Forward)
        else {
            panic!("expected Incompatible");
        };
        assert!(msg.starts_with("forward:"), "{msg}");
    }

    #[test]
    fn compatibility_refuses_to_guess_when_the_comparison_exceeds_its_budget() {
        let text = seq(&format!(
            "{}{}",
            el("a", "xs:string", ""),
            el("b", "xs:string", r#"minOccurs="0""#)
        ));
        let (old, new) = (compile_text(&text).unwrap(), compile_text(&text).unwrap());
        let mut tight = Inclusion {
            sup: &new,
            sub: &old,
            sup_label: "new",
            sub_label: "old",
            visited: HashSet::new(),
            pairs: 0,
            max_pairs: 1,
            budget: Budget::new(MAX_COMPAT_WORK),
            depth: 0,
        };
        let err = tight.roots().unwrap_err();
        assert!(err.contains("too complex to compare"), "{err}");
        // The same schemas are proven compatible within the real budget.
        assert_eq!(included(&new, &old, "new", "old"), Ok(()));
    }

    #[test]
    fn a_refused_new_required_element_is_named_in_a_stable_phrase() {
        let old = seq(&format!(
            "{}{}",
            el("a", "xs:string", ""),
            el("tail", "xs:string", r#"maxOccurs="unbounded""#)
        ));
        let new = seq(&format!(
            "{}{}{}",
            el("a", "xs:string", ""),
            el("termin", "xs:date", ""),
            el("tail", "xs:string", r#"maxOccurs="unbounded""#)
        ));
        let Err(SchemaError::Incompatible(msg)) =
            XSD_OPS.check_compatibility(&old, &new, Compatibility::Backward)
        else {
            panic!("expected Incompatible");
        };
        assert!(msg.contains("requires element 'termin'"), "{msg}");
        // Ending early while the other schema still wants an element.
        let old = seq(&el("a", "xs:string", ""));
        let new = seq(&format!(
            "{}{}",
            el("a", "xs:string", ""),
            el("termin", "xs:date", "")
        ));
        let Err(SchemaError::Incompatible(msg)) =
            XSD_OPS.check_compatibility(&old, &new, Compatibility::Backward)
        else {
            panic!("expected Incompatible");
        };
        assert!(msg.contains("requires element 'termin'"), "{msg}");
        // Several candidates are listed together.
        let new = element_with(&format!(
            "<xs:sequence>{}<xs:choice>{}{}</xs:choice></xs:sequence>",
            el("a", "xs:string", ""),
            el("x", "xs:string", ""),
            el("y", "xs:string", "")
        ));
        let Err(SchemaError::Incompatible(msg)) =
            XSD_OPS.check_compatibility(&old, &new, Compatibility::Backward)
        else {
            panic!("expected Incompatible");
        };
        assert!(
            msg.contains("requires one of the elements 'x', 'y'"),
            "{msg}"
        );
    }

    #[test]
    fn a_wide_optional_sequence_is_compared_within_a_work_budget() {
        let items: String = (0..2000)
            .map(|i| el(&format!("c{i}"), "xs:string", r#"minOccurs="0""#))
            .collect();
        let text = seq(&items);
        let (old, new) = (compile_text(&text).unwrap(), compile_text(&text).unwrap());
        let started = std::time::Instant::now();
        let got = included(&new, &old, "new", "old");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "comparison ran for {:?}",
            started.elapsed()
        );
        // Either proven within the budget or refused as too complex, never an
        // unbounded run or a wrong refusal.
        assert!(
            got == Ok(()) || got.as_ref().is_err_and(|m| m.contains("too complex")),
            "{got:?}"
        );
        // The budget really bites on the product construction.
        let mut spent = Inclusion {
            sup: &new,
            sub: &old,
            sup_label: "new",
            sub_label: "old",
            visited: HashSet::new(),
            pairs: 0,
            max_pairs: MAX_COMPAT_PAIRS,
            budget: Budget {
                used: MAX_COMPAT_WORK,
                limit: MAX_COMPAT_WORK,
            },
            depth: 0,
        };
        let err = spent.roots().unwrap_err();
        assert!(err.contains("too complex to compare"), "{err}");
        assert_eq!(
            spent.budget.used, MAX_COMPAT_WORK,
            "a refused step is not added"
        );
    }

    /// `n` named complex types, each holding one child of the next one.
    fn complex_chain(n: usize, extra: &str) -> String {
        let mut body = String::new();
        for i in 0..n {
            if i + 1 < n {
                body.push_str(&format!(
                    r#"<xs:complexType name="n{i}"><xs:sequence><xs:element name="c" type="n{}"/></xs:sequence></xs:complexType>"#,
                    i + 1
                ));
            } else {
                body.push_str(&format!(r#"<xs:complexType name="n{i}"/>"#));
            }
        }
        schema(&format!(r#"{extra}{body}<xs:element name="r" type="n0"/>"#))
    }

    #[test]
    fn a_long_named_complex_chain_is_too_complex_to_compare_not_incompatible() {
        let note = "<xs:annotation><xs:documentation>x</xs:documentation></xs:annotation>";
        let short = (complex_chain(10, ""), complex_chain(10, note));
        assert!(XSD_OPS
            .check_compatibility(&short.0, &short.1, Compatibility::Full)
            .is_ok());
        let long = (
            complex_chain(MAX_COMPAT_DEPTH + 10, ""),
            complex_chain(MAX_COMPAT_DEPTH + 10, note),
        );
        for mode in [
            Compatibility::Backward,
            Compatibility::Forward,
            Compatibility::Full,
        ] {
            let got = XSD_OPS.check_compatibility(&long.0, &long.1, mode);
            assert!(
                matches!(&got, Err(SchemaError::LimitExceeded(m)) if m.contains("too complex to compare")),
                "{mode:?}: {got:?}"
            );
        }
    }

    #[test]
    fn chains_of_derived_simple_types_are_bounded() {
        let stacked = |n: usize, reverse: bool| {
            let mut types: Vec<String> = (0..n)
                .map(|i| {
                    let base = if i == 0 {
                        "xs:string".to_string()
                    } else {
                        format!("t{}", i - 1)
                    };
                    format!(
                        r#"<xs:simpleType name="t{i}"><xs:restriction base="{base}"><xs:maxLength value="{}"/></xs:restriction></xs:simpleType>"#,
                        1000 - i
                    )
                })
                .collect();
            if reverse {
                types.reverse();
            }
            schema(&format!(
                r#"{}<xs:element name="r" type="t{}"/>"#,
                types.concat(),
                n - 1
            ))
        };
        assert!(ops_compile(&stacked(MAX_SCHEMA_DEPTH, false)).is_ok());
        let Err(SchemaError::Invalid(m)) = ops_compile(&stacked(MAX_SCHEMA_DEPTH + 8, false))
        else {
            panic!("a long chain of stacked restrictions must be refused");
        };
        assert!(m.contains("stacked through more than"), "{m}");
        // Declared in reverse, the first type pulls the whole chain in at
        // once: bounded by the nesting of named bases.
        let Err(SchemaError::Invalid(m)) = ops_compile(&stacked(MAX_SCHEMA_DEPTH + 8, true)) else {
            panic!("a deep chain of named bases must be refused");
        };
        assert!(m.contains("chain of more than"), "{m}");
    }

    // ---- derived schema against the XML projection ----------------------------

    fn projected_is_valid_under_derived(
        schema_text: &str,
        allowed: &[&str],
        doc: &str,
    ) -> Result<(), SchemaError> {
        let original = compiled(schema_text);
        XSD_OPS
            .validate(&original, doc.as_bytes())
            .expect("the source document must be valid before projection");
        let allowed = set(allowed);
        let projected = PayloadFormat::Xml
            .codec()
            .project(doc.as_bytes(), &allowed)
            .unwrap();
        let derived = XSD_OPS.derive_subschema(schema_text, &allowed).unwrap();
        XSD_OPS.validate(&compiled(&derived), &projected)
    }

    #[test]
    fn derived_schema_accepts_projections_of_prefixed_documents() {
        let namespaced = format!(
            r#"<xs:schema xmlns:xs="{XSD_NS}" targetNamespace="urn:x"><xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="a" type="xs:string"/><xs:element name="b" type="xs:string"/>
               </xs:sequence></xs:complexType></xs:element></xs:schema>"#
        );
        // The policy names `a` unprefixed; the document spells it `p:a`, so the
        // projection drops it and the derived schema must not insist on it.
        let doc = r#"<p:r xmlns:p="urn:x"><p:a>1</p:a><p:b>2</p:b></p:r>"#;
        assert_eq!(
            projected_is_valid_under_derived(&namespaced, &["a"], doc),
            Ok(())
        );

        // A prefixed policy entry keeps only that exact spelling.
        let plain = schema(
            r#"<xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="a" type="xs:string"/><xs:element name="b" type="xs:string"/>
               </xs:sequence></xs:complexType></xs:element>"#,
        );
        assert_eq!(
            projected_is_valid_under_derived(&plain, &["p:a"], "<r><a>1</a><b>2</b></r>"),
            Ok(())
        );
        // Without namespaces and with a bare entry the requirement is kept.
        let derived = XSD_OPS.derive_subschema(&plain, &set(&["a"])).unwrap();
        assert!(XSD_OPS.validate(&compiled(&derived), b"<r></r>").is_err());
        assert_eq!(
            projected_is_valid_under_derived(&plain, &["a"], "<r><a>1</a><b>2</b></r>"),
            Ok(())
        );
    }

    #[test]
    fn derived_schema_accepts_a_root_whose_text_the_projection_drops() {
        let int_root = schema(r#"<xs:element name="r" type="xs:int"/>"#);
        assert_eq!(
            projected_is_valid_under_derived(&int_root, &[], "<r>5</r>"),
            Ok(())
        );
        // Text restricted by its type, which the projection empties.
        let with_attribute = schema_with_attribute()
            .replace("name=\"a\"", "name=\"r\"")
            .replace("base=\"xs:string\"", "base=\"xs:int\"");
        assert_eq!(
            projected_is_valid_under_derived(&with_attribute, &[], r#"<r ward="w">5</r>"#),
            Ok(())
        );
        // The root's attributes survive the projection and stay declared.
        let derived = XSD_OPS
            .derive_subschema(&with_attribute, &set(&[]))
            .unwrap();
        assert!(derived.contains(r#"name="ward""#));
    }

    // ---- misc ---------------------------------------------------------------

    #[test]
    fn validate_refuses_a_foreign_compiled_schema() {
        let other = CompiledSchema::StoredOnly(super::super::SchemaType::Avro);
        assert!(matches!(
            XSD_OPS.validate(&other, b"<a/>"),
            Err(SchemaError::Invalid(_))
        ));
    }

    // ---- compatibility cost, shared facets, class ranges, payload names -----

    /// Same attributes in both schemas; the first schema's shared type
    /// enumerates `members`, the second's accepts them through `patterns`
    /// decoys followed by the one pattern that matches.
    fn attribute_pair(count: usize, members: usize, patterns: usize) -> (String, String) {
        let attrs: String = (0..count)
            .map(|i| format!(r#"<xs:attribute name="a{i}" type="T"/>"#))
            .collect();
        let enumeration: String = (0..members)
            .map(|i| format!(r#"<xs:enumeration value="v{i}"/>"#))
            .collect();
        let decoys: String = (0..patterns)
            .map(|i| format!(r#"<xs:pattern value="q{i}x"/>"#))
            .chain(std::iter::once(
                r#"<xs:pattern value="v[0-9]+"/>"#.to_string(),
            ))
            .collect();
        let doc = |facets: &str| {
            schema(&format!(
                r#"<xs:simpleType name="T"><xs:restriction base="xs:string">{facets}</xs:restriction></xs:simpleType>
                <xs:element name="r"><xs:complexType>{attrs}</xs:complexType></xs:element>"#
            ))
        };
        (doc(&enumeration), doc(&decoys))
    }

    #[test]
    fn simple_type_comparison_is_charged_to_the_compatibility_budget() {
        let (members, patterns) = attribute_pair(500, 2048, 255);
        // Backward compares the new schema (accepting) against the old one.
        assert!(members.len() < MAX_SCHEMA_TEXT_BYTES && patterns.len() < MAX_SCHEMA_TEXT_BYTES);
        let started = std::time::Instant::now();
        let result = XSD_OPS.check_compatibility(&members, &patterns, Compatibility::Backward);
        assert!(
            matches!(&result, Err(SchemaError::LimitExceeded(m)) if m.contains("too complex")),
            "{result:?}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let (small_members, small_patterns) = attribute_pair(3, 4, 2);
        XSD_OPS
            .check_compatibility(&small_members, &small_patterns, Compatibility::Backward)
            .unwrap();
    }

    #[test]
    fn derived_types_share_the_facets_of_their_base() {
        let enumeration: String = (0..100)
            .map(|i| format!(r#"<xs:enumeration value="v{i}"/>"#))
            .collect();
        let derived: String = (0..300)
            .map(|i| {
                format!(
                    r#"<xs:simpleType name="D{i}"><xs:restriction base="B"><xs:maxLength value="{}"/></xs:restriction></xs:simpleType>"#,
                    10 + i
                )
            })
            .collect();
        let elements: String = (0..300)
            .map(|i| el(&format!("e{i}"), &format!("D{i}"), ""))
            .collect();
        let text = schema(&format!(
            r#"<xs:simpleType name="B"><xs:restriction base="xs:string">{enumeration}</xs:restriction></xs:simpleType>
            {derived}<xs:element name="r"><xs:complexType><xs:sequence>{elements}</xs:sequence></xs:complexType></xs:element>"#
        ));
        let CompiledSchema::Xsd(c) = compiled(&text) else {
            panic!("expected an xsd schema");
        };
        let bases: Vec<&Arc<Facets>> = c
            .types
            .iter()
            .filter_map(|t| match t {
                TypeDef::Simple(s) if s.steps.len() == 2 => Some(&s.steps[0]),
                _ => None,
            })
            .collect();
        assert_eq!(bases.len(), 300);
        assert!(bases.iter().all(|b| Arc::ptr_eq(b, bases[0])));
        assert_eq!(bases[0].enumeration.len(), 100);
    }

    #[test]
    fn multi_character_escapes_cannot_bound_a_class_range() {
        for bad in [r"[\s-~]", r"[\t-\s]", r"[\d-z]", r"[a-\d]", r"[\p{L}-z]"] {
            assert!(build_pattern(bad).is_err(), "{bad} must be refused");
        }
        for good in [
            r"[\s]", r"[-\s]", r"[\s-]", r"[a-z\s]", r"[\sa-z]", r"[\d\s]",
        ] {
            assert!(build_pattern(good).is_ok(), "{good} must be accepted");
        }
    }

    #[test]
    fn payload_names_are_shortened_in_violations() {
        let text = seq(&el("a", "xs:string", ""));
        let long = "n".repeat(1_000_000);
        let wide = "é".repeat(1_000_000);
        for name in [&long, &wide] {
            for doc in [
                format!(r#"<r {name}="1"><a/></r>"#),
                format!(r#"<r xsi:{name}="1"><a/></r>"#),
                format!(r#"<{name}/>"#),
            ] {
                let msg = violation_of(&text, &doc);
                assert!(msg.chars().count() < 600, "{}", msg.chars().count());
            }
        }
        let typed = element_with(r#"<xs:sequence/><xs:attribute name="x" type="xs:int"/>"#);
        let msg = violation_of(&typed, &format!(r#"<r {long}="1"/>"#));
        assert!(msg.chars().count() < 600);
    }

    // ---- structural resource bounds -------------------------------------------

    fn within(started: std::time::Instant, secs: u64) {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(secs),
            "took {:?}",
            started.elapsed()
        );
    }

    fn refusal_of(text: &str) -> String {
        match ops_compile(text) {
            Err(SchemaError::Invalid(m)) => m,
            other => panic!("expected a compile refusal, got {other:?}"),
        }
    }

    fn string_type(facets: &str) -> String {
        format!(
            r#"<xs:simpleType name="T"><xs:restriction base="xs:string">{facets}</xs:restriction></xs:simpleType>"#
        )
    }

    fn attributes_of(count: usize) -> String {
        (0..count)
            .map(|i| format!(r#"<xs:attribute name="a{i}" type="T"/>"#))
            .collect()
    }

    fn root_with_attributes(types: &str, count: usize) -> String {
        schema(&format!(
            r#"{types}<xs:element name="r"><xs:complexType>{}</xs:complexType></xs:element>"#,
            attributes_of(count)
        ))
    }

    #[test]
    fn a_huge_enumeration_member_is_refused_at_compile_before_any_comparison() {
        let member = format!("v{}", "0".repeat(128 * 1024));
        let old = root_with_attributes(
            &string_type(&format!(r#"<xs:enumeration value="{member}"/>"#)),
            2500,
        );
        let decoys: String = (0..255)
            .map(|i| format!(r#"<xs:pattern value="q{i}x"/>"#))
            .collect();
        let new = root_with_attributes(&string_type(&decoys), 2500);
        assert!(old.len() < MAX_SCHEMA_TEXT_BYTES && new.len() < MAX_SCHEMA_TEXT_BYTES);
        let started = std::time::Instant::now();
        for mode in [Compatibility::Backward, Compatibility::Full] {
            let got = XSD_OPS.check_compatibility(&old, &new, mode);
            assert!(
                matches!(&got, Err(SchemaError::Invalid(m)) if m.contains("an enumeration value is longer than 1024 bytes")),
                "{got:?}"
            );
        }
        within(started, 10);
    }

    #[test]
    fn compatibility_of_maximal_enumerations_against_many_patterns_is_limited() {
        // Everything at its cap: 1024 attributes, 1 KiB members, 255 patterns
        // of which only the last one accepts.
        let members: String = (0..4)
            .map(|i| format!(r#"<xs:enumeration value="v{i}{}"/>"#, "0".repeat(1000)))
            .collect();
        let decoys: String = (0..254)
            .map(|i| format!(r#"<xs:pattern value="q{i}x"/>"#))
            .chain(std::iter::once(
                r#"<xs:pattern value="v[0-9]+"/>"#.to_string(),
            ))
            .collect();
        let old = root_with_attributes(&string_type(&members), MAX_ATTRIBUTES_PER_TYPE);
        let new = root_with_attributes(&string_type(&decoys), MAX_ATTRIBUTES_PER_TYPE);
        let started = std::time::Instant::now();
        let got = XSD_OPS.check_compatibility(&old, &new, Compatibility::Backward);
        assert!(
            matches!(&got, Err(SchemaError::LimitExceeded(m)) if m.contains("too complex to compare")),
            "{got:?}"
        );
        within(started, 10);
    }

    /// Deterministic pseudo-random 0/1 text (xorshift), `len` bytes.
    fn random_bits(len: usize) -> String {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        (0..len)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                if x & 1 == 1 {
                    '1'
                } else {
                    '0'
                }
            })
            .collect()
    }

    #[test]
    fn a_value_that_cannot_be_matched_within_the_budget_is_limited_not_scanned() {
        let heavy = r#"<xs:pattern value="[01]*1[01]{200}"/>"#;
        let many = string_type(&heavy.repeat(MAX_PATTERNS));
        let s = schema(&format!(r#"{many}<xs:element name="r" type="T"/>"#));
        let c = compiled(&s);
        // Bits that never repeat a window keep the lazy DFA building new
        // states until it gives up; a periodic string would run in a few
        // hundred cached states.
        let value = random_bits(4 * 1024 * 1024);
        let started = std::time::Instant::now();
        let got = XSD_OPS.validate(&c, format!("<r>{value}</r>").as_bytes());
        assert!(
            matches!(&got, Err(SchemaError::LimitExceeded(m)) if m.contains("validation work budget")),
            "{got:?}"
        );
        within(started, 5);

        // One such pattern over a megabyte is bounded as well: once the lazy
        // DFA gives up, the Pike VM costs the automaton width per byte, which
        // is charged before it runs.
        let one = schema(&format!(
            r#"{}<xs:element name="r" type="T"/>"#,
            string_type(heavy)
        ));
        let started = std::time::Instant::now();
        let got = XSD_OPS.validate(
            &compiled(&one),
            format!("<r>{}</r>", random_bits(1024 * 1024)).as_bytes(),
        );
        assert!(
            matches!(&got, Err(SchemaError::LimitExceeded(_))),
            "{got:?}"
        );
        within(started, 5);

        // A value that fits the budget is still checked for real.
        let got = XSD_OPS.validate(&compiled(&one), b"<r>0101</r>");
        assert!(matches!(got, Err(SchemaError::Violation(_))), "{got:?}");
        let mut ok = String::from("1");
        ok.push_str(&"0".repeat(200));
        assert!(accepts(&one, &format!("<r>{ok}</r>")));
    }

    #[test]
    fn many_attributes_on_repeated_elements_validate_in_linear_time() {
        let types = string_type("");
        let s = schema(&format!(
            r#"{types}<xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="e" minOccurs="0" maxOccurs="unbounded"><xs:complexType>{}</xs:complexType></xs:element>
               </xs:sequence></xs:complexType></xs:element>"#,
            attributes_of(MAX_ATTRIBUTES_PER_TYPE)
        ));
        let c = compiled(&s);
        let one: String = (0..MAX_ATTRIBUTES_PER_TYPE)
            .map(|i| format!(r#" a{i}="1""#))
            .collect();
        let doc = format!("<r>{}</r>", format!("<e{one}/>").repeat(500));
        let started = std::time::Instant::now();
        XSD_OPS.validate(&c, doc.as_bytes()).unwrap();
        within(started, 10);

        // The same document against a tight budget is limited, not ignored.
        let CompiledSchema::Xsd(inner) = &c else {
            panic!("expected an xsd schema");
        };
        let got = validate_with(inner, doc.as_bytes(), &mut Budget::new(1_000_000));
        assert!(
            matches!(&got, Err(SchemaError::LimitExceeded(m)) if m.contains("validation work budget")),
            "{got:?}"
        );

        // 5700 attributes the type never declared stop at the first one, and a
        // repeated one is malformed however many there are.
        let wide: String = (0..5700).map(|i| format!(r#" b{i}="1""#)).collect();
        let started = std::time::Instant::now();
        assert!(violation_of(&s, &format!("<r><e{wide}/></r>")).contains("is not declared"));
        let dup: String = (0..5700).map(|i| format!(r#" a{}="1""#, i % 900)).collect();
        assert!(violation_of(&s, &format!("<r><e{dup}/></r>")).contains("malformed attribute"));
        within(started, 5);
    }

    #[test]
    fn a_type_with_more_attributes_than_the_cap_is_refused() {
        let ok = root_with_attributes(&string_type(""), MAX_ATTRIBUTES_PER_TYPE);
        assert!(ops_compile(&ok).is_ok());
        let too_many = root_with_attributes(&string_type(""), MAX_ATTRIBUTES_PER_TYPE + 1);
        assert!(refusal_of(&too_many).contains("a type declares more than 1024 attributes"));
    }

    #[test]
    fn patterns_that_need_too_much_memory_are_refused_at_compile() {
        // Each PESEL-sized pattern is fine alone ...
        let pesel = r#"<xs:pattern value="\d{11}"/>"#;
        let one = schema(&format!(
            r#"{}<xs:element name="r" type="T"/>"#,
            string_type(pesel)
        ));
        assert!(ops_compile(&one).is_ok());
        // ... but a schema full of them is not.
        let started = std::time::Instant::now();
        let many = schema(&format!(
            r#"{}<xs:element name="r" type="T"/>"#,
            string_type(&pesel.repeat(MAX_PATTERNS))
        ));
        let msg = refusal_of(&many);
        assert!(
            msg.contains(&format!(
                "the patterns of the schema need more than {} KiB of memory",
                MAX_PATTERN_MEMORY / 1024
            )),
            "{msg}"
        );
        // One automaton past the largest tier is refused on its own.
        let big = schema(&format!(
            r#"{}<xs:element name="r" type="T"/>"#,
            string_type(r#"<xs:pattern value="\p{L}{60}"/>"#)
        ));
        assert!(refusal_of(&big).contains("the compiled pattern is too large"));
        within(started, 10);
    }

    #[test]
    fn enumeration_caps_are_refused_with_pinned_phrases() {
        let value = |n: usize| format!(r#"<xs:enumeration value="{}"/>"#, "x".repeat(n));
        let at_cap = schema(&format!(
            r#"{}<xs:element name="r" type="T"/>"#,
            string_type(&value(MAX_ENUM_VALUE_BYTES))
        ));
        assert!(ops_compile(&at_cap).is_ok());
        let long = schema(&format!(
            r#"{}<xs:element name="r" type="T"/>"#,
            string_type(&value(MAX_ENUM_VALUE_BYTES + 1))
        ));
        assert!(refusal_of(&long).contains("an enumeration value is longer than 1024 bytes"));

        let total: String = (0..MAX_ENUM_TOTAL_BYTES / 512 + 1)
            .map(|i| format!(r#"<xs:enumeration value="{i:0>512}"/>"#))
            .collect();
        let heavy = schema(&format!(
            r#"{}<xs:element name="r" type="T"/>"#,
            string_type(&total)
        ));
        assert!(heavy.len() < MAX_SCHEMA_TEXT_BYTES + 64 * 1024);
        let msg = refusal_of(&heavy);
        assert!(
            msg.contains("the enumeration values of the schema hold more than 128 KiB in total")
                || msg.contains("byte limit"),
            "{msg}"
        );

        let count: String = (0..=MAX_ENUMERATION_VALUES)
            .map(|i| format!(r#"<xs:enumeration value="{i}"/>"#))
            .collect();
        let many = schema(&format!(
            r#"{}<xs:element name="r" type="T"/>"#,
            string_type(&count)
        ));
        assert!(refusal_of(&many).contains("the schema lists more than 4096 enumeration values"));
    }

    #[test]
    fn a_full_enumeration_checks_many_short_values_in_bounded_time() {
        let members: String = (0..MAX_ENUMERATION_VALUES)
            .map(|i| format!(r#"<xs:enumeration value="c{i}"/>"#))
            .collect();
        let s = schema(&format!(
            r#"{}<xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="v" type="T" minOccurs="0" maxOccurs="unbounded"/>
               </xs:sequence></xs:complexType></xs:element>"#,
            string_type(&members)
        ));
        let c = compiled(&s);
        let doc_of = |count: usize| {
            format!(
                "<r>{}</r>",
                (0..count)
                    .map(|i| format!("<v>c{}</v>", i % MAX_ENUMERATION_VALUES))
                    .collect::<String>()
            )
        };
        // A realistic count is accepted within the budget ...
        let started = std::time::Instant::now();
        let got = XSD_OPS.validate(&c, doc_of(60_000).as_bytes());
        assert_eq!(got, Ok(()));
        within(started, 10);
        // ... and an adversarial one is limited, not scanned to the end.
        let started = std::time::Instant::now();
        let got = XSD_OPS.validate(&c, doc_of(1_200_000).as_bytes());
        assert!(
            matches!(&got, Err(SchemaError::LimitExceeded(m)) if m.contains("validation work budget")),
            "{got:?}"
        );
        within(started, 20);
        let bad = "<r><v>c1</v><v>nope</v></r>";
        assert!(violation_of(&s, bad).contains("is not one of the enumerated values"));
    }

    #[test]
    fn enumeration_intersection_is_charged_before_it_is_computed() {
        let step = |names: std::ops::Range<usize>| {
            Arc::new(Facets {
                enumeration: names.map(|i| format!("v{i}")).collect(),
                ..Facets::default()
            })
        };
        let ty = SimpleType {
            builtin: Builtin::String,
            steps: vec![step(0..2048), step(0..2048)],
        };
        // Charging the second step alone (4096) overruns this budget, so the
        // intersection never runs.
        let mut tight = Budget::new(5000);
        assert_eq!(ty.effective_enumeration(&mut tight), Err(LimitExceeded));
        assert!(tight.used <= tight.limit, "a refused charge is not added");
        let mut roomy = Budget::new(1_000_000);
        let members = ty.effective_enumeration(&mut roomy).unwrap().unwrap();
        assert_eq!(members.len(), 2048);
        assert!(members.windows(2).all(|w| w[0] <= w[1]), "sorted");
    }

    #[test]
    fn check_cannot_run_without_a_budget_and_stops_when_it_is_spent() {
        let CompiledSchema::Xsd(c) = compiled(&schema(&format!(
            r#"{}<xs:element name="r" type="T"/>"#,
            string_type(r#"<xs:pattern value="a+"/><xs:maxLength value="10"/>"#)
        ))) else {
            panic!("expected an xsd schema");
        };
        let ty = c.simple(c.elements[c.roots["r"]].ty);
        assert_eq!(ty.check("aaa", &mut Budget::new(1_000)), Ok(()));
        assert_eq!(
            ty.check("aaa", &mut Budget::new(0)),
            Err(CheckFailure::TooComplex)
        );
        assert_eq!(
            ty.check("bbb", &mut Budget::new(1_000)),
            Err(violated("does not match the pattern"))
        );
        let mut b = Budget::new(10);
        assert_eq!(b.charge(u64::MAX), Err(LimitExceeded));
        assert_eq!(b.charge(u64::MAX), Err(LimitExceeded), "cannot wrap");
    }

    #[test]
    fn a_charge_that_does_not_fit_is_refused_without_being_added() {
        let mut b = Budget::new(10);
        b.charge(4).unwrap();
        assert_eq!(b.charge(7), Err(LimitExceeded));
        assert_eq!(b.used, 4, "check before commit");
        b.charge(6).unwrap();
        assert_eq!(b.used, 10);
    }

    #[test]
    fn a_record_over_its_allowance_does_not_drain_the_batch() {
        let pattern = "[01]*1[01]{200}";
        let s = schema(&format!(
            r#"{}<xs:element name="r" type="T"/>"#,
            string_type(&format!(r#"<xs:pattern value="{pattern}"/>"#))
        ));
        let c = compiled(&s);
        // The give-up charge (cache + length x width) of this record dwarfs
        // the whole allowance; it must not be debited from the batch.
        let hostile = format!("<r>{}</r>", random_bits(300_000));
        let honest = format!("<r>1{}</r>", "0".repeat(200));
        let mut shared = ValidationBudget::for_batch(hostile.len() + 2 * honest.len());
        let before = shared.remaining;
        let first = XSD_OPS.validate_metered(&c, hostile.as_bytes(), &mut shared);
        assert!(
            matches!(&first, Err(SchemaError::LimitExceeded(_))),
            "{first:?}"
        );
        assert!(
            before - shared.remaining < 5_000_000,
            "only the work that ran is debited: {}",
            before - shared.remaining
        );
        XSD_OPS
            .validate_metered(&c, honest.as_bytes(), &mut shared)
            .unwrap();
        XSD_OPS
            .validate_metered(&c, honest.as_bytes(), &mut shared)
            .unwrap();
    }

    // ---- accurate pattern charging, non-capturing groups, accounting --------

    fn oracle(pattern: &str) -> regex::Regex {
        regex::Regex::new(&translate_pattern(pattern).expect(pattern)).expect(pattern)
    }

    #[test]
    fn xsd_groups_never_capture() {
        for pattern in ["(ab)", "((a)(b))+", "(a|b)(c|d)", "()", &"()".repeat(255)] {
            let translated = translate_pattern(pattern).unwrap();
            assert_eq!(
                regex::Regex::new(&translated).unwrap().captures_len(),
                1,
                "{pattern}: every group must be (?:...)"
            );
            let matcher = build_pattern(pattern).unwrap();
            assert_eq!(
                matcher
                    .dfa
                    .get_nfa()
                    .group_info()
                    .group_len(regex_automata::PatternID::ZERO),
                1
            );
        }
    }

    #[test]
    fn the_automata_engine_agrees_with_the_regex_crate_on_the_xsd_dialect() {
        let cases: [(&str, &[&str]); 12] = [
            ("[a-c]+", &["", "abc", "abcd", "xabc"]),
            (r"\d{3}", &["123", "12", "1234", "١٢٣", "a23"]),
            (r"\p{Lu}\p{L}*", &["Ábc", "ábc", "A", ""]),
            ("a.c", &["abc", "a\nc", "a\rc", "ac", "aéc"]),
            ("a|b", &["a", "b", "ab", ""]),
            ("(ab){2,3}", &["ab", "abab", "ababab", "abababab"]),
            ("^a$", &["^a$", "a"]),
            (r"[a\-z]", &["-", "a", "z", "b"]),
            ("[^a-c]", &["d", "a", "é", ""]),
            (
                r"PL\d{26}",
                &[
                    "PL61109010140000071219812874",
                    "PL6110901014000007121981287",
                ],
            ),
            (r"\d{2}-\d{3}", &["00-950", "00950", "0-9500"]),
            (r"\s+x?", &[" ", "\t\n", " x", "x"]),
        ];
        for (pattern, values) in cases {
            let matcher = build_pattern(pattern).unwrap();
            let expected = oracle(pattern);
            for value in values {
                assert_eq!(
                    matcher.is_match(value, &mut Budget::new(u64::MAX)).unwrap(),
                    expected.is_match(value),
                    "{pattern} on {value:?}"
                );
            }
        }
    }

    #[test]
    fn a_pattern_that_keeps_the_lazy_dfa_cached_costs_one_unit_per_byte() {
        let matcher = build_pattern(r"\d+").unwrap();
        let value = "7".repeat(1_000_000);
        let mut budget = Budget::new(u64::MAX);
        assert!(matcher.is_match(&value, &mut budget).unwrap());
        let scanned = 1 + value.len() as u64;
        assert!(
            (scanned..scanned + scanned / 100).contains(&budget.used),
            "one unit per byte plus the states built once: {} vs {scanned}",
            budget.used
        );
    }

    #[test]
    fn a_thrashing_lazy_dfa_falls_back_to_a_pike_vm_charged_by_automaton_width() {
        let pattern = "[01]*1[01]{200}";
        let matcher = build_pattern(pattern).unwrap();
        let value = random_bits(100_000);
        let mut unlimited = Budget::new(u64::MAX);
        assert_eq!(
            matcher.is_match(&value, &mut unlimited).unwrap(),
            oracle(pattern).is_match(&value),
            "the fallback decides like the regex crate"
        );
        let states = matcher.dfa.get_nfa().states().len() as u64;
        assert!(
            unlimited.used >= value.len() as u64 * states,
            "the Pike VM costs states x length ({} vs {})",
            unlimited.used,
            value.len() as u64 * states
        );
        let mut tight = Budget::new(10_000_000);
        assert_eq!(matcher.is_match(&value, &mut tight), Err(LimitExceeded));
    }

    #[test]
    fn the_pike_fallback_is_charged_by_the_unicode_width_of_the_automaton() {
        let narrow = build_pattern("[01]*1[01]{200}").unwrap();
        let wide = build_pattern(r"\p{L}+\d{3}").unwrap();
        for matcher in [&narrow, &wide] {
            assert!(matcher.width >= matcher.dfa.get_nfa().states().len() as u64);
        }
        let states = wide.dfa.get_nfa().states().len() as u64;
        assert!(
            wide.width > 3 * states,
            "Unicode classes are sparse transitions of many ranges: {} vs {states} states",
            wide.width
        );
    }

    /// Eight validators share one pattern: every match gets its own cache from
    /// the pool, the caches are reused, and the work stays near what is charged.
    #[test]
    fn concurrent_matches_of_one_pattern_do_bounded_work_near_their_charge() {
        let cases: [(&str, String); 2] = [
            ("[01]*1[01]{200}", random_bits(600)),
            (r"\p{L}+\d{3}", format!("{}123", "żółć".repeat(150))),
        ];
        for (pattern, value) in cases {
            let matcher = Arc::new(build_pattern(pattern).unwrap());
            let started = std::time::Instant::now();
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    let matcher = Arc::clone(&matcher);
                    let value = value.clone();
                    std::thread::spawn(move || {
                        let began = std::time::Instant::now();
                        let mut budget = Budget::new(u64::MAX);
                        for _ in 0..100 {
                            matcher.is_match(&value, &mut budget).unwrap();
                        }
                        (began.elapsed(), budget.used)
                    })
                })
                .collect();
            let (mut cpu, mut units) = (std::time::Duration::ZERO, 0u64);
            for t in threads {
                let (elapsed, used) = t.join().unwrap();
                cpu += elapsed;
                units += used;
            }
            let ns_per_unit = cpu.as_nanos() as f64 / units as f64;
            eprintln!(
                "{pattern}: wall {:?}, {units} units, {ns_per_unit:.1} ns/unit",
                started.elapsed()
            );
            // A unit is documented as ~10 ns; allow generous headroom for
            // loaded CI machines and debug builds, but not an unmetered
            // multiple (the old per-match cache rebuilds cost far more).
            let ceiling = if cfg!(debug_assertions) { 400.0 } else { 100.0 };
            assert!(
                ns_per_unit < ceiling,
                "{pattern}: {ns_per_unit} ns per unit"
            );
            within(started, 60);
        }
    }

    #[test]
    fn building_a_fresh_cache_is_charged_once_per_cache() {
        let matcher = build_pattern(r"\p{L}+\d{3}").unwrap();
        let states = matcher.dfa.get_nfa().states().len() as u64;
        let mut first = Budget::new(u64::MAX);
        matcher.is_match("abc123", &mut first).unwrap();
        assert!(first.used >= 1 + 6 + states, "{} vs {states}", first.used);
        let mut warm = Budget::new(u64::MAX);
        matcher.is_match("abc123", &mut warm).unwrap();
        assert!(
            warm.used < first.used - states / 2,
            "the warm cache is not charged again"
        );
    }

    #[test]
    fn the_mark_table_of_a_record_is_charged_up_front() {
        let CompiledSchema::Xsd(c) = compiled(&wide_choice_schema(4990)) else {
            panic!("expected an xsd schema");
        };
        let mut budget = Budget::new(MAX_VALIDATION_STEPS);
        validate_with(&c, b"<r/>", &mut budget).unwrap();
        assert!(
            budget.used >= c.max_nfa_states as u64 / 8,
            "{} vs {}",
            budget.used,
            c.max_nfa_states
        );
    }

    /// 10k records of a realistic national-identifier shape.
    fn identifier_schema() -> String {
        let pattern = |name: &str, p: &str| {
            format!(
                r#"<xs:simpleType name="{name}"><xs:restriction base="xs:string"><xs:pattern value="{p}"/></xs:restriction></xs:simpleType>"#
            )
        };
        let types = [
            pattern("Pesel", r"\d{11}"),
            pattern("Nip", r"\d{10}"),
            pattern("Postcode", r"\d{2}-\d{3}"),
            pattern("Iban", r"PL\d{26}"),
        ]
        .concat();
        schema(&format!(
            r#"{types}<xs:element name="batch"><xs:complexType><xs:sequence>
                 <xs:element name="rec" minOccurs="0" maxOccurs="unbounded"><xs:complexType><xs:sequence>
                   <xs:element name="pesel" type="Pesel"/><xs:element name="nip" type="Nip"/>
                   <xs:element name="postcode" type="Postcode"/><xs:element name="iban" type="Iban"/>
                 </xs:sequence></xs:complexType></xs:element>
               </xs:sequence></xs:complexType></xs:element>"#
        ))
    }

    fn identifier_batch(records: usize) -> String {
        let body: String =
            (0..records)
                .map(|i| {
                    format!(
                    "<rec><pesel>{:011}</pesel><nip>{:010}</nip><postcode>{:02}-{:03}</postcode>\
                     <iban>PL{:026}</iban></rec>",
                    i, i * 7, i % 100, i % 1000, i * 13
                )
                })
                .collect();
        format!("<batch>{body}</batch>")
    }

    #[test]
    fn a_realistic_batch_of_identifiers_validates_within_the_work_budget() {
        let c = compiled(&identifier_schema());
        let CompiledSchema::Xsd(inner) = &c else {
            panic!("expected an xsd schema");
        };
        let doc = identifier_batch(10_000);
        let started = std::time::Instant::now();
        let mut budget = Budget::new(MAX_VALIDATION_STEPS);
        validate_with(inner, doc.as_bytes(), &mut budget).unwrap();
        eprintln!(
            "10k identifier records ({} KiB): {:?}, {} units",
            doc.len() / 1024,
            started.elapsed(),
            budget.used
        );
        assert!(
            budget.used < MAX_VALIDATION_STEPS / 4,
            "a realistic batch must leave most of the budget: {}",
            budget.used
        );
        within(started, 30);
        // The same schema still tells a bad record from a good one.
        let bad = identifier_batch(3).replace("PL0000000000000000000000000", "PLx");
        assert!(violation_of(&identifier_schema(), &bad).contains("does not match the pattern"));
    }

    #[test]
    fn thousands_of_ibans_in_one_document_validate() {
        let s = schema(&format!(
            r#"{}<xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="iban" type="T" minOccurs="0" maxOccurs="unbounded"/>
               </xs:sequence></xs:complexType></xs:element>"#,
            string_type(r#"<xs:pattern value="PL\d{26}"/>"#)
        ));
        let c = compiled(&s);
        let doc = format!(
            "<r>{}</r>",
            (0..20_000)
                .map(|i| format!("<iban>PL{:026}</iban>", i * 31))
                .collect::<String>()
        );
        let started = std::time::Instant::now();
        XSD_OPS.validate(&c, doc.as_bytes()).unwrap();
        eprintln!("20k IBANs: {:?}", started.elapsed());
        within(started, 30);
    }

    #[test]
    fn pattern_memory_covers_the_program_and_the_pooled_caches() {
        let matcher = build_pattern(r"\d{11}").unwrap();
        assert!(
            matcher.memory_bytes()
                >= matcher.dfa.get_nfa().memory_usage() + POOLED_CACHES * PATTERN_CACHE_FLOOR
        );
        let one = schema(&format!(
            r#"{}<xs:element name="r" type="T"/>"#,
            string_type(r#"<xs:pattern value="\d{11}"/>"#)
        ));
        let CompiledSchema::Xsd(inner) = compiled(&one) else {
            panic!("expected an xsd schema");
        };
        assert!(inner.extra_bytes() >= matcher.memory_bytes());
    }

    /// A type whose content is a `minOccurs="0"` choice of `alternatives`
    /// elements, inside a root that repeats it.
    fn wide_choice_schema(alternatives: usize) -> String {
        let alts: String = (0..alternatives)
            .map(|i| format!(r#"<xs:element name="a{i}" type="xs:string"/>"#))
            .collect();
        schema(&format!(
            r#"<xs:complexType name="X"><xs:choice minOccurs="0">{alts}</xs:choice></xs:complexType>
               <xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="x" type="X" minOccurs="0" maxOccurs="unbounded"/>
               </xs:sequence></xs:complexType></xs:element>"#
        ))
    }

    #[test]
    fn the_automaton_of_a_content_model_counts_into_the_cache_weight() {
        let CompiledSchema::Xsd(inner) = compiled(&wide_choice_schema(4990)) else {
            panic!("expected an xsd schema");
        };
        assert!(
            inner.extra_bytes() >= 4990 * 2 * std::mem::size_of::<NfaState>(),
            "{}",
            inner.extra_bytes()
        );
    }

    #[test]
    fn entering_an_element_with_a_wide_start_set_is_charged_per_element() {
        let s = wide_choice_schema(4990);
        let c = compiled(&s);
        let started = std::time::Instant::now();
        let doc = format!("<r>{}</r>", "<x/>".repeat(20_000));
        let got = XSD_OPS.validate(&c, doc.as_bytes());
        assert!(
            matches!(&got, Err(SchemaError::LimitExceeded(m)) if m.contains("validation work budget")),
            "{got:?}"
        );
        within(started, 10);
        // A handful of the same elements is fine, and a real child still works.
        assert!(accepts(&s, "<r><x/><x><a17>v</a17></x><x/></r>"));
    }

    #[test]
    fn entering_an_all_group_element_is_charged_per_declared_child() {
        let children: String = (0..1000)
            .map(|i| format!(r#"<xs:element name="c{i}" type="xs:string" minOccurs="0"/>"#))
            .collect();
        let s = schema(&format!(
            r#"<xs:complexType name="X"><xs:all>{children}</xs:all></xs:complexType>
               <xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="x" type="X" minOccurs="0" maxOccurs="unbounded"/>
               </xs:sequence></xs:complexType></xs:element>"#
        ));
        let c = compiled(&s);
        let doc = format!("<r>{}</r>", "<x/>".repeat(70_000));
        let got = XSD_OPS.validate(&c, doc.as_bytes());
        assert!(
            matches!(&got, Err(SchemaError::LimitExceeded(m)) if m.contains("validation work budget")),
            "{got:?}"
        );
        assert!(accepts(&s, "<r><x/><x><c3>v</c3></x></r>"));
    }

    #[test]
    fn a_shared_budget_is_spent_across_documents() {
        let c = compiled(&wide_choice_schema(4990));
        // About 30M units per document, below the per-document cap.
        let doc = format!("<r>{}</r>", "<x/>".repeat(6_000));
        XSD_OPS.validate(&c, doc.as_bytes()).unwrap();
        let mut shared = ValidationBudget {
            remaining: 40_000_000,
        };
        XSD_OPS
            .validate_metered(&c, doc.as_bytes(), &mut shared)
            .unwrap();
        assert!(shared.remaining < 15_000_000, "{}", shared.remaining);
        let second = XSD_OPS.validate_metered(&c, doc.as_bytes(), &mut shared);
        assert!(
            matches!(&second, Err(SchemaError::LimitExceeded(_))),
            "{second:?}"
        );
        assert!(
            shared.remaining < 5_000,
            "only the work that fitted was debited: {}",
            shared.remaining
        );
        let third = XSD_OPS.validate_metered(&c, b"<r><x/></r>", &mut shared);
        assert!(
            matches!(&third, Err(SchemaError::LimitExceeded(_))),
            "{third:?}"
        );
    }

    #[test]
    fn a_policy_matches_prefixed_entries_by_their_local_name() {
        let allowed: BTreeSet<String> = ["ns:a", "b"].iter().map(|s| s.to_string()).collect();
        let kept = Kept::new(&allowed);
        assert!(kept.keeps("a") && kept.keeps("ns:a") && kept.keeps("b"));
        assert!(!kept.keeps("c") && !kept.keeps("ns"));
    }

    #[test]
    fn nfa_state_sets_keep_the_accept_state_first() {
        let CompiledSchema::Xsd(c) = compiled(&schema(
            r#"<xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="a" type="xs:string" minOccurs="0"/>
                 <xs:element name="b" type="xs:string" minOccurs="0"/>
               </xs:sequence></xs:complexType></xs:element>"#,
        )) else {
            panic!("expected an xsd schema");
        };
        let TypeDef::Complex(ComplexType {
            content: Content::Model(Model::Nfa(m)),
            ..
        }) = &c.types[c.elements[c.roots["r"]].ty]
        else {
            panic!("expected an automaton model");
        };
        let nfa = &m.nfa;
        assert_eq!(nfa.start_set[0], NFA_ACCEPT, "both children are optional");
        assert!(nfa.accepts(&nfa.start_set));
        let mut marks = Marks::new(nfa.states.len());
        let mut budget = Budget::new(u64::MAX);
        let after_a = nfa
            .step(&nfa.start_set, m.by_name["a"], &mut marks, &mut budget)
            .unwrap();
        assert_eq!(after_a.iter().filter(|&&s| s == NFA_ACCEPT).count(), 1);
        assert_eq!(after_a[0], NFA_ACCEPT);
        let after_ab = nfa
            .step(&after_a, m.by_name["b"], &mut marks, &mut budget)
            .unwrap();
        assert!(nfa.accepts(&after_ab));
        let after_aa = nfa
            .step(&after_a, m.by_name["a"], &mut marks, &mut budget)
            .unwrap();
        assert!(after_aa.is_empty(), "a second a is not allowed");

        // Whatever order the walk finds it in, the accept state ends up first.
        let hand_built = Nfa {
            states: vec![
                NfaState::default(),
                NfaState {
                    eps: Vec::new(),
                    edge: Some((0, 0)),
                },
                NfaState {
                    eps: vec![NFA_ACCEPT],
                    edge: None,
                },
            ],
            start_set: Vec::new(),
        };
        let mut out = Vec::new();
        hand_built
            .closure(&[2, 1], &mut out, &mut Marks::new(3), &mut Budget::new(100))
            .unwrap();
        assert_eq!(out, [NFA_ACCEPT, 1]);
        assert!(hand_built.accepts(&out));
    }

    /// The largest `[01]*1[01]{n}` the compiler admits: an exploding automaton
    /// at the top of the program size limit.
    fn largest_exploding_pattern() -> (String, Matcher) {
        let mut best = None;
        let mut n = 1000;
        while n <= 12_000 {
            let source = format!("[01]*1[01]{{{n}}}");
            match build_pattern(&source) {
                Ok(matcher) => best = Some((source, matcher)),
                Err(_) => break,
            }
            n += 1000;
        }
        best.expect("a thousand-state pattern compiles")
    }

    #[test]
    fn a_top_size_exploding_pattern_is_refused_quickly_over_a_quarter_megabyte() {
        let (source, matcher) = largest_exploding_pattern();
        let states = matcher.dfa.get_nfa().states().len();
        let schema_text = schema(&format!(
            r#"{}<xs:element name="r" type="T"/>"#,
            string_type(&format!(r#"<xs:pattern value="{source}"/>"#))
        ));
        let c = compiled(&schema_text);
        let doc = format!("<r>{}</r>", random_bits(256 * 1024));
        let started = std::time::Instant::now();
        let got = XSD_OPS.validate(&c, doc.as_bytes());
        eprintln!(
            "{source}: {states} NFA states, {} bytes of program, 256 KiB random value refused in {:?}",
            matcher.dfa.get_nfa().memory_usage(),
            started.elapsed()
        );
        assert!(
            matches!(&got, Err(SchemaError::LimitExceeded(m)) if m.contains("validation work budget")),
            "{got:?}"
        );
        within(started, 5);

        // The same pattern over many values that each fit the budget alone
        // (and match) is limited by their total, not by their count.
        let n: usize = source
            .trim_start_matches("[01]*1[01]{")
            .trim_end_matches('}')
            .parse()
            .unwrap();
        let values = format!(
            "<r>{}</r>",
            (0..40)
                .map(|_| format!("<v>1{}</v>", random_bits(n)))
                .collect::<String>()
        );
        let many = schema(&format!(
            r#"{}<xs:element name="r"><xs:complexType><xs:sequence>
                 <xs:element name="v" type="T" minOccurs="0" maxOccurs="unbounded"/>
               </xs:sequence></xs:complexType></xs:element>"#,
            string_type(&format!(r#"<xs:pattern value="{source}"/>"#))
        ));
        let started = std::time::Instant::now();
        let got = XSD_OPS.validate(&compiled(&many), values.as_bytes());
        eprintln!(
            "40 matching values of {n} bytes: {got:?} in {:?}",
            started.elapsed()
        );
        assert!(
            matches!(&got, Err(SchemaError::LimitExceeded(_))),
            "{got:?}"
        );
        within(started, 5);
    }
}
