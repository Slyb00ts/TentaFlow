// =============================================================================
// File: bus/schema_registry/protobuf.rs — Protocol Buffers schemas (F4 B2)
// =============================================================================
// SUM/tentabus/PLAN-F4-REST.md §B.2. The pure-Rust `.proto` parser of
// `protobuf-parse` (no `protoc`) reads the SCHEMA; everything that touches
// untrusted bytes or decides compatibility runs on a small representation
// lowered from its descriptor, because neither the parser nor the library's
// dynamic message decoder is safe on these paths (audit, measured on
// protobuf-parse 3.7.2):
//   - the parser is recursive, with no depth limit: a few thousand nested
//     messages, option aggregates (`{a{a{...`) or list constants (`[[[[...`)
//     overflow a 2 MiB stack and abort the process (it is not a panic that
//     could be caught);
//   - its work grows with the square of the declarations: resolving a type
//     name scans every sibling declaration and allocates a vector per scope
//     (20 000 messages: 0.86 s; 2 000 messages with 8 references each: 1.8 s);
//   - two messages or enums of one name and different members make an
//     `unwrap` in its option resolver panic, and so does a `map<K, V> a` next
//     to a type named `AEntry` (or two maps whose names camel-case alike, or a
//     group next to a type of its name): the release profile is
//     `panic = "abort"`, so a panic there ends the process and `catch_unwind`
//     around the call only helps dev and test builds. The pre-scan therefore
//     refuses every one of those collisions BEFORE the parser runs, and a
//     mutation fuzz (900 000 mutated schemas, structural and lexical) found
//     no panic past it;
//   - it resolves `import` against embedded copies of the well-known types,
//     keeps `service`, `extend`, groups and extension ranges, and accepts
//     what `protoc` refuses (a proto3 enum that does not start at zero, a
//     float map key, numbers outside 1..2^29-1);
//   - it reads files, so the text is staged in a private temporary directory
//     (the in-memory resolver is not exported), and its error text names
//     that path;
//   - the dynamic decoder (`protobuf::reflect`) is bounded by its input (a
//     declared length is checked against the bytes left before the vector
//     is filled, with at most 10 MB reserved ahead) but it builds the whole
//     message in memory (an allocation per field and per nested message), has
//     no work meter to share across a publish batch, recurses on the native
//     stack to its limit of 100, accepts a `bool` of 2, a closed-enum value
//     nobody declared and the numbers 19000-19999, and says "Unexpected wire
//     type" without naming a field. It stays in the tests as an oracle (what
//     it refuses, we must refuse), and payloads are walked by our own decoder.
//
// Schema text accepted: ONE `.proto` file, `syntax = "proto3"` or `"proto2"`
// (no `syntax` line means proto2, as in protoc; `edition` is refused).
// Refused with a fixed phrase: `import`, `service`, `extend`, `extensions`,
// groups, custom options (`option (x) = ...`), `message_set_wire_format`,
// more than 64 `required` fields in one message. Allowed: nested messages and
// enums, `oneof`, `map<K, V>`, `optional`, `repeated`, `reserved`, `package`,
// and the options protoc itself interprets (they do not change the wire).
// Well-known types need an `import`, so they are refused with it.
//
// The designated message (the one every payload of the subject is an instance
// of) is the FIRST top-level message of the file, or the top-level message
// named by the comment `// tentabus:message <Name>`. This needs no wire change
// to the register request, and it survives the derived projection (the
// designated message is written first).
//
// Field defaults (proto2 `default`), `json_name`, `packed` and the codegen file
// options are read and ignored: none of them changes what is on the wire.
//
// Resource bounds are structural, as in `avro.rs` and `xsd.rs`:
//   - `prescan` runs on the TEXT before the library sees it: it tokenizes with
//     the lexer's own rules (comments end at `\n`, `/* */` does not nest,
//     strings do not span lines) and refuses what would overflow or slow the
//     parser: bracket depth over `MAX_BRACKET_DEPTH`, more than
//     `MAX_ASSIGNMENTS` `=`, `MAX_MESSAGES` messages (map fields count, they
//     become messages), `MAX_ENUMS` enums, identifiers over `MAX_NAME_LEN`,
//     strings over `MAX_STRING_LITERAL`, `(` anywhere (custom options),
//     non-ASCII outside strings and comments, a type name (message, enum or
//     the entry of a `map`) declared twice in one scope, and `import` /
//     `service` / `extend` / `extensions` / `group` used as the keyword that
//     starts a statement or a field.
//   - after parsing, the descriptor is checked against the same caps exactly
//     (messages, nesting, fields per message and in total, enum values), and
//     against what protoc validates and the parser does not.
//   - `validate` is an iterative walk with an explicit stack of at most
//     `MAX_VALUE_DEPTH` frames (no recursion, so no stack depth to exhaust)
//     and NO per-value allocation. Every length is checked against the bytes
//     left in its message BEFORE anything is read or skipped; a packed run is
//     charged by its byte length before it is iterated; every field costs a
//     unit of a `Budget` (a document cap shared by a publish batch through
//     `ValidationBudget`). Running out is `LimitExceeded`, never a verdict.
//   - `check_compatibility` runs on the same representation under its own
//     `Budget` with a depth cap and a memo of message pairs; what it cannot
//     decide in budget is `LimitExceeded`, never "compatible".
//
// Wire format accepted by `validate`: ONE serialized message per record
// payload, the designated message. An empty payload is a valid message (every
// field absent) unless the schema has a `required` field. Strict where the
// descriptor says so: a declared field must arrive with its type's wire type
// (a packed run is accepted for repeated numeric fields, in proto2 as in
// proto3), `int32`/`enum`/`uint32`/`sint32` values must fit, `bool` is 0 or 1,
// `string` is UTF-8, a CLOSED (proto2) enum value must be declared, a
// `required` field must be present. Lenient where protobuf is: a field number
// the schema does not declare is skipped (that is how versions evolve; the
// walk still checks its framing), a singular field may repeat (the last wins),
// several members of one `oneof` may arrive, an open (proto3) enum accepts
// any int32. Always refused: field number 0 or above 2^29-1, the range
// 19000-19999 the implementation reserves, the deprecated group wire types
// (3, 4) and the invalid ones (6, 7), a varint over ten bytes.
//
// Compatibility (`reads(writer, reader)`: every message written under `writer`
// can be read under `reader`; BACKWARD = `reads(old, new)`, FORWARD =
// `reads(new, old)`, FULL = both). A field's identity is its NUMBER; names do
// not matter on the wire. A field present on both sides must keep its type
// (every scalar kind is its own type: `int32` -> `int64` is refused although
// the wire type matches, and so is `string` -> `bytes` or a singular field
// that becomes `repeated`), enum stays enum, message stays message (compared
// recursively, by field numbers), map stays map. A field only the writer has
// is skipped by the reader, a field only the reader has is absent in the data:
// both fine, EXCEPT a `required` reader field, which the writer must also
// declare `required`. A closed reader enum (proto2) needs every value the
// writer enum declares. Independently of direction, a field number the OLD
// schema lists in `reserved` may not appear in the NEW one. Removing a field
// without reserving its number is allowed (the reader skips it).
//
// `derive_subschema` keeps the designated message's allowed top-level fields
// (by name) and every message and enum type they reach, nested where they were
// nested, and writes a `.proto` file that is checked to compile again.
//
// Violation messages are PATH + CONSTRAINT only (path = field names from the
// designated message, `<root>` for the message itself) — never a payload
// value; they reach audit rows, warn logs and DLQ headers.
// =============================================================================

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;

use ::protobuf::descriptor::field_descriptor_proto::{Label, Type};
use ::protobuf::descriptor::{DescriptorProto, EnumDescriptorProto, FileDescriptorProto};

use super::{
    shorten_path, Budget, Compatibility, CompiledSchema, LimitExceeded, SchemaError, SchemaKindOps,
    ValidationBudget, MAX_SCHEMA_TEXT_BYTES,
};

/// `{` and `[` open together: the parser recurses through message bodies,
/// option aggregates and list constants alike.
const MAX_BRACKET_DEPTH: usize = 32;
const MAX_MESSAGE_NESTING: usize = 16;
/// Messages including the entry type every `map` field becomes.
const MAX_MESSAGES: usize = 256;
const MAX_ENUMS: usize = 128;
const MAX_FIELDS_PER_MESSAGE: usize = 1024;
const MAX_FIELDS: usize = 4096;
const MAX_ENUM_VALUES_PER_ENUM: usize = 1024;
const MAX_ENUM_VALUES: usize = 4096;
const MAX_RESERVED_RANGES: usize = 1024;
const MAX_REQUIRED_FIELDS: usize = 64;
/// `=` outside comments and strings: fields, enum values, options and
/// `[...]` field options all use one, so this bounds the parser's work.
const MAX_ASSIGNMENTS: usize = 12_288;
/// Identifiers: messages, fields, enums, packages (a package is dotted, each
/// part counts). Messages quote them, so the cap bounds every message.
const MAX_NAME_LEN: usize = 128;
const MAX_STRING_LITERAL: usize = 256;
const MAX_PARSER_MESSAGE: usize = 200;

const MAX_FIELD_NUMBER: u32 = 536_870_911;
const RESERVED_NUMBERS: std::ops::RangeInclusive<u32> = 19_000..=19_999;
const DESIGNATION_PREFIX: &str = "tentabus:message";

/// Nesting of messages a payload may reach. Recursive schemas make the depth
/// data-dependent, so it is capped at check time.
const MAX_VALUE_DEPTH: usize = 64;
/// Work units one document may spend: about 5 ns each, so ~0.1 s at most.
const MAX_VALIDATION_UNITS: u64 = 20_000_000;
const MAX_COMPAT_WORK: u64 = 1_000_000;
const MAX_COMPAT_DEPTH: usize = 64;

const TRUNCATED: &str = "payload ends before the value is complete";
const INT_TOO_LONG: &str = "integer encoding is longer than 10 bytes or overflows 64 bits";
const LENGTH_EXCEEDS: &str = "declared length exceeds the remaining payload";
const BAD_UTF8: &str = "string is not valid UTF-8";
const BAD_BOOLEAN: &str = "boolean is neither 0 nor 1";
const TOO_DEEP: &str = "value is nested too deeply to check";
const VALIDATION_BUDGET: &str = "document exceeds the validation work budget";
const FIELD_NUMBER: &str = "field number is out of range";
const FIELD_RESERVED: &str =
    "field number is in the range reserved for the protobuf implementation";
const WIRE_MISMATCH: &str = "wire type does not match the type declared for the field";
const GROUP_WIRE: &str = "group wire types are not supported";
const BAD_WIRE: &str = "wire type is not valid";
const PACKED_SIZE: &str = "packed field does not hold a whole number of elements";
const VALUE_RANGE: &str = "value does not fit the declared type";
const ENUM_UNDECLARED: &str = "enum value is not declared in the closed enum";
const REQUIRED_MISSING: &str = "required field is missing";
/// What a refusal says when the parser panicked anyway; nothing the pre-scan lets
/// through may produce it (`mutated_schema_texts_never_panic_and_stay_fast`).
const PARSER_PANIC: &str = "not a valid Protobuf schema: the parser could not read the text";
const TOO_COMPLEX: &str = "the schemas are too complex to compare; compatibility cannot be proven";

fn invalid(msg: impl Into<String>) -> SchemaError {
    SchemaError::Invalid(msg.into())
}

// =============================================================================
// Lowered schema
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scalar {
    Double,
    Float,
    Int32,
    Int64,
    Uint32,
    Uint64,
    Sint32,
    Sint64,
    Fixed32,
    Fixed64,
    Sfixed32,
    Sfixed64,
    Bool,
    String,
    Bytes,
}

impl Scalar {
    fn name(self) -> &'static str {
        match self {
            Scalar::Double => "double",
            Scalar::Float => "float",
            Scalar::Int32 => "int32",
            Scalar::Int64 => "int64",
            Scalar::Uint32 => "uint32",
            Scalar::Uint64 => "uint64",
            Scalar::Sint32 => "sint32",
            Scalar::Sint64 => "sint64",
            Scalar::Fixed32 => "fixed32",
            Scalar::Fixed64 => "fixed64",
            Scalar::Sfixed32 => "sfixed32",
            Scalar::Sfixed64 => "sfixed64",
            Scalar::Bool => "bool",
            Scalar::String => "string",
            Scalar::Bytes => "bytes",
        }
    }

    fn from_type(ty: Type) -> Option<Scalar> {
        Some(match ty {
            Type::TYPE_DOUBLE => Scalar::Double,
            Type::TYPE_FLOAT => Scalar::Float,
            Type::TYPE_INT32 => Scalar::Int32,
            Type::TYPE_INT64 => Scalar::Int64,
            Type::TYPE_UINT32 => Scalar::Uint32,
            Type::TYPE_UINT64 => Scalar::Uint64,
            Type::TYPE_SINT32 => Scalar::Sint32,
            Type::TYPE_SINT64 => Scalar::Sint64,
            Type::TYPE_FIXED32 => Scalar::Fixed32,
            Type::TYPE_FIXED64 => Scalar::Fixed64,
            Type::TYPE_SFIXED32 => Scalar::Sfixed32,
            Type::TYPE_SFIXED64 => Scalar::Sfixed64,
            Type::TYPE_BOOL => Scalar::Bool,
            Type::TYPE_STRING => Scalar::String,
            Type::TYPE_BYTES => Scalar::Bytes,
            Type::TYPE_ENUM | Type::TYPE_MESSAGE | Type::TYPE_GROUP => return None,
        })
    }

    /// Types a `map` key may have (protoc: integral, bool or string).
    fn is_map_key(self) -> bool {
        !matches!(self, Scalar::Double | Scalar::Float | Scalar::Bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Scalar(Scalar),
    Enum(usize),
    Message(usize),
}

/// The wire type a value of this type is written with on its own.
fn wire_type(ty: Ty) -> u8 {
    match ty {
        Ty::Scalar(Scalar::Double | Scalar::Fixed64 | Scalar::Sfixed64) => 1,
        Ty::Scalar(Scalar::Float | Scalar::Fixed32 | Scalar::Sfixed32) => 5,
        Ty::Scalar(Scalar::String | Scalar::Bytes) | Ty::Message(_) => 2,
        Ty::Scalar(_) | Ty::Enum(_) => 0,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cardinality {
    Optional,
    Required,
    Repeated,
}

#[derive(Debug)]
struct Field {
    name: String,
    number: u32,
    cardinality: Cardinality,
    ty: Ty,
    /// A real `oneof` (not the synthetic one of a proto3 `optional`).
    oneof: Option<usize>,
    /// proto3 `optional`: written back with the keyword.
    explicit_optional: bool,
    /// Bit in the per-message "seen" mask of a `required` field.
    required_bit: Option<u32>,
}

#[derive(Debug, Default)]
struct Msg {
    name: String,
    /// Dotted name from the package, no leading dot: how types are written
    /// back out.
    full_name: String,
    parent: Option<usize>,
    /// Declaration order.
    fields: Vec<Field>,
    /// `(number, index into fields)`, sorted by number.
    by_number: Vec<(u32, usize)>,
    oneofs: Vec<String>,
    /// Inclusive `(first, last)`.
    reserved: Vec<(u32, u32)>,
    map_entry: bool,
    /// Declared nested messages (map entry types excluded) and enums.
    nested: Vec<usize>,
    enums: Vec<usize>,
    required_mask: u64,
}

impl Msg {
    fn field_by_number(&self, number: u32) -> Option<usize> {
        self.by_number
            .binary_search_by_key(&number, |(n, _)| *n)
            .ok()
            .map(|i| self.by_number[i].1)
    }

    fn is_reserved(&self, number: u32) -> bool {
        in_ranges(&self.reserved, number)
    }
}

#[derive(Debug)]
struct EnumDef {
    name: String,
    full_name: String,
    parent: Option<usize>,
    /// Declaration order.
    values: Vec<(String, i32)>,
    /// Sorted, deduplicated numbers.
    numbers: Vec<i32>,
    /// proto2 enums are closed: an undeclared number is not a value.
    closed: bool,
    allow_alias: bool,
}

#[derive(Debug)]
pub struct Compiled {
    msgs: Vec<Msg>,
    enums: Vec<EnumDef>,
    root: usize,
    top_messages: Vec<usize>,
    top_enums: Vec<usize>,
    package: String,
    proto3: bool,
}

impl Compiled {
    fn field_type_label(&self, field: &Field) -> String {
        let base = match field.ty {
            Ty::Scalar(s) => s.name().to_string(),
            Ty::Enum(e) => format!("enum {}", self.enums[e].name),
            Ty::Message(m) if self.msgs[m].map_entry => {
                let entry = &self.msgs[m];
                format!(
                    "map<{}, {}>",
                    self.type_name(entry.fields[0].ty),
                    self.type_name(entry.fields[1].ty)
                )
            }
            Ty::Message(m) => format!("message {}", self.msgs[m].name),
        };
        if field.cardinality == Cardinality::Repeated && !self.is_map(field) {
            format!("repeated {base}")
        } else {
            base
        }
    }

    fn type_name(&self, ty: Ty) -> String {
        match ty {
            Ty::Scalar(s) => s.name().to_string(),
            Ty::Enum(e) => self.enums[e].name.clone(),
            Ty::Message(m) => self.msgs[m].name.clone(),
        }
    }

    fn is_map(&self, field: &Field) -> bool {
        matches!(field.ty, Ty::Message(m) if self.msgs[m].map_entry)
    }
}

// =============================================================================
// Text pre-scan
// =============================================================================

struct Prescan {
    designated: Option<String>,
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The name of the message the parser makes for the `map<K, V> name` whose `<`
/// is at `open`: the field name in upper camel case plus `Entry`.
fn map_entry_name(text: &str, open: usize) -> Option<String> {
    let rest = &text[open + 1..];
    let close = rest.as_bytes().iter().take(1024).position(|&b| b == b'>')?;
    let after = rest[close + 1..].trim_start();
    let end = after.bytes().take_while(|b| is_ident_byte(*b)).count();
    let field = &after[..end];
    if field.is_empty() || field.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let mut name = String::with_capacity(field.len() + 5);
    let mut capitalize = true;
    for c in field.chars() {
        if c == '_' {
            capitalize = true;
        } else if capitalize {
            name.push(c.to_ascii_uppercase());
            capitalize = false;
        } else {
            name.push(c);
        }
    }
    name.push_str("Entry");
    Some(name)
}

/// Walks the text with the lexer's own rules and refuses what would overflow
/// the parser's stack or what it handles in quadratic time (see the header).
fn prescan(text: &str) -> Result<Prescan, SchemaError> {
    let b = text.as_bytes();
    let mut i = 0;
    let mut depth = 0usize;
    let mut assignments = 0usize;
    let mut messages = 0usize;
    let mut enums = 0usize;
    // True at the start of a statement: only there are `message`, `import` &
    // co. keywords for the parser.
    let mut statement_start = true;
    let mut previous_ident: Option<&str> = None;
    let mut designated: Option<String> = None;
    // Type names declared in each open `{`: the parser panics (an `unwrap` in
    // its option resolver) on two messages or enums of one name, so a repeat
    // is refused here with its own sentence.
    let mut scopes: Vec<HashSet<String>> = vec![HashSet::new()];
    while i < b.len() {
        let c = b[i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                let end = b[i..]
                    .iter()
                    .position(|&x| x == b'\n')
                    .map_or(b.len(), |p| i + p);
                let comment = text[i + 2..end].trim();
                if let Some(rest) = comment.strip_prefix(DESIGNATION_PREFIX) {
                    if rest.is_empty() || rest.starts_with(char::is_whitespace) {
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
                        if designated.replace(name.to_string()).is_some() {
                            return Err(invalid(
                                "the comment '// tentabus:message' appears more than once",
                            ));
                        }
                    }
                }
                i = end;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let Some(close) = text[i + 2..].find("*/") else {
                    return Err(invalid("a comment is never closed"));
                };
                i += 2 + close + 2;
            }
            b'"' | b'\'' => {
                let quote = c;
                let start = i + 1;
                i += 1;
                loop {
                    match b.get(i) {
                        None | Some(b'\n') | Some(0) => {
                            return Err(invalid("a string is never closed on its line"))
                        }
                        Some(b'\\') => {
                            if matches!(b.get(i + 1), None | Some(b'\n')) {
                                return Err(invalid("a string is never closed on its line"));
                            }
                            i += 2;
                        }
                        Some(&q) if q == quote => break,
                        Some(_) => i += 1,
                    }
                }
                if i - start > MAX_STRING_LITERAL {
                    return Err(invalid(format!(
                        "a string is longer than {MAX_STRING_LITERAL} bytes"
                    )));
                }
                i += 1;
                statement_start = false;
                previous_ident = None;
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < b.len() && is_ident_byte(b[i]) {
                    i += 1;
                }
                if i - start > MAX_NAME_LEN {
                    return Err(invalid(format!(
                        "a name is longer than {MAX_NAME_LEN} characters"
                    )));
                }
                let word = &text[start..i];
                if statement_start {
                    // The keyword is told from an enum value or a type that
                    // happens to share its spelling by what follows it.
                    let next = b[i..].iter().find(|c| !c.is_ascii_whitespace()).copied();
                    let starts_name =
                        next.is_some_and(|c| c.is_ascii_alphabetic() || c == b'_' || c == b'.');
                    match word {
                        "group" if next.is_some_and(|c| c.is_ascii_uppercase()) => {
                            return Err(invalid("groups are not supported"))
                        }
                        "import"
                            if depth == 0
                                && (starts_name || matches!(next, Some(b'"' | b'\''))) =>
                        {
                            return Err(invalid(
                                "import is not supported: a schema must be one self-contained file",
                            ))
                        }
                        "service" if depth == 0 && starts_name => {
                            return Err(invalid("service is not supported"))
                        }
                        "extend" if starts_name => return Err(invalid("extend is not supported")),
                        "extensions" if next.is_some_and(|c| c.is_ascii_digit()) => {
                            return Err(invalid("extension ranges are not supported"))
                        }
                        "message" | "enum" => {
                            if word == "message" {
                                messages += 1;
                            } else {
                                enums += 1;
                            }
                            let name_start = i + b[i..]
                                .iter()
                                .take_while(|c| c.is_ascii_whitespace())
                                .count();
                            let name_len = b[name_start..]
                                .iter()
                                .take_while(|c| is_ident_byte(**c))
                                .count();
                            let name = &text[name_start..name_start + name_len];
                            if !name.is_empty()
                                && !name.as_bytes()[0].is_ascii_digit()
                                && !scopes.last_mut().expect("a scope").insert(name.to_string())
                            {
                                return Err(invalid(format!(
                                    "type '{name}' is defined more than once"
                                )));
                            }
                        }
                        _ => {}
                    }
                }
                if word == "group"
                    && matches!(previous_ident, Some("optional" | "required" | "repeated"))
                {
                    return Err(invalid("groups are not supported"));
                }
                statement_start = false;
                previous_ident = Some(word);
            }
            c if c.is_ascii_digit() => {
                while i < b.len() && (is_ident_byte(b[i]) || b[i] == b'.') {
                    i += 1;
                }
                statement_start = false;
                previous_ident = None;
            }
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_BRACKET_DEPTH {
                    return Err(invalid(format!(
                        "the schema is nested deeper than {MAX_BRACKET_DEPTH} levels"
                    )));
                }
                statement_start = c == b'{';
                if c == b'{' {
                    scopes.push(HashSet::new());
                }
                previous_ident = None;
                i += 1;
            }
            b'}' | b']' => {
                if depth == 0 {
                    return Err(invalid("a closing bracket has no opening one"));
                }
                depth -= 1;
                if c == b'}' && scopes.len() > 1 {
                    scopes.pop();
                }
                statement_start = c == b'}';
                previous_ident = None;
                i += 1;
            }
            b';' => {
                statement_start = true;
                previous_ident = None;
                i += 1;
            }
            b'=' => {
                assignments += 1;
                if assignments > MAX_ASSIGNMENTS {
                    return Err(invalid(format!(
                        "the schema has more than {MAX_ASSIGNMENTS} assignments (field numbers, enum values and options)"
                    )));
                }
                statement_start = false;
                previous_ident = None;
                i += 1;
            }
            b'<' => {
                if previous_ident == Some("map") {
                    messages += 1;
                    // The parser declares `<CamelName>Entry` next to the other
                    // types of the scope, and panics when that name is taken.
                    if let Some(entry) = map_entry_name(text, i) {
                        if !scopes.last_mut().expect("a scope").insert(entry.clone()) {
                            return Err(invalid(format!(
                                "type '{entry}' is defined more than once"
                            )));
                        }
                    }
                }
                statement_start = false;
                previous_ident = None;
                i += 1;
            }
            b'(' => {
                return Err(invalid("custom options are not supported"));
            }
            b'>' | b',' | b'.' | b':' | b'-' | b'+' | b'/' => {
                statement_start = false;
                previous_ident = None;
                i += 1;
            }
            _ => {
                return Err(invalid(
                    "the schema contains a character a .proto file cannot",
                ))
            }
        }
        if messages > MAX_MESSAGES {
            return Err(invalid(format!(
                "the schema has more than {MAX_MESSAGES} messages (a map field counts as one)"
            )));
        }
        if enums > MAX_ENUMS {
            return Err(invalid(format!(
                "the schema has more than {MAX_ENUMS} enums"
            )));
        }
    }
    if depth != 0 {
        return Err(invalid("a bracket is never closed"));
    }
    Ok(Prescan { designated })
}

// =============================================================================
// Parsing and lowering
// =============================================================================

/// The library's text for an error, without the staging path and bounded.
fn parser_failure(error: &anyhow::Error, staged: &str) -> SchemaError {
    let chain: Vec<String> = error.chain().map(|e| e.to_string()).collect();
    let line = chain.iter().find_map(|text| {
        let rest = &text[text.find("at ")? + 3..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        (rest[digits.len()..].starts_with(':')).then_some(digits)
    });
    let root = error.root_cause().to_string().replace(staged, "<schema>");
    let root: String = root.chars().take(MAX_PARSER_MESSAGE).collect();
    match line {
        Some(line) if !line.is_empty() && line.len() < 8 => {
            invalid(format!("not a valid Protobuf schema (line {line}): {root}"))
        }
        _ => invalid(format!("not a valid Protobuf schema: {root}")),
    }
}

fn run_parser(text: &str) -> Result<FileDescriptorProto, SchemaError> {
    let dir = tempfile::Builder::new()
        .prefix("tf-proto-")
        .tempdir()
        .map_err(|_| invalid("the schema could not be staged for parsing"))?;
    let file = dir.path().join("schema.proto");
    std::fs::write(&file, text)
        .map_err(|_| invalid("the schema could not be staged for parsing"))?;
    let staged = dir.path().display().to_string();
    let parsed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ::protobuf_parse::Parser::new()
            .pure()
            .include(dir.path())
            .input(&file)
            .parse_and_typecheck()
    }));
    let parsed = match parsed {
        Err(_) => return Err(invalid(PARSER_PANIC)),
        Ok(Err(e)) => return Err(parser_failure(&e, &staged)),
        Ok(Ok(parsed)) => parsed,
    };
    let mut files = parsed.file_descriptors;
    if files.len() != 1 {
        return Err(invalid(
            "import is not supported: a schema must be one self-contained file",
        ));
    }
    Ok(files.remove(0))
}

struct Lowering<'a> {
    msgs: Vec<Msg>,
    enums: Vec<EnumDef>,
    ids: HashMap<String, Ty>,
    pending: Vec<(usize, &'a DescriptorProto)>,
    proto3: bool,
    fields: usize,
    enum_values: usize,
}

/// Sorted, non-overlapping inclusive ranges, so a number is found by bisection.
fn merge_ranges(mut ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    ranges.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for (first, last) in ranges {
        match merged.last_mut() {
            Some(prev) if first <= prev.1.saturating_add(1) => prev.1 = prev.1.max(last),
            _ => merged.push((first, last)),
        }
    }
    merged
}

fn in_ranges(merged: &[(u32, u32)], number: u32) -> bool {
    let after = merged.partition_point(|(first, _)| *first <= number);
    after > 0 && merged[after - 1].1 >= number
}

fn join(scope: &str, name: &str) -> String {
    if scope.is_empty() {
        name.to_string()
    } else {
        format!("{scope}.{name}")
    }
}

fn check_no_custom_options(count: usize) -> Result<(), SchemaError> {
    if count == 0 {
        Ok(())
    } else {
        Err(invalid("custom options are not supported"))
    }
}

impl<'a> Lowering<'a> {
    fn register(&mut self, full_name: String, ty: Ty) -> Result<(), SchemaError> {
        if self.ids.insert(full_name.clone(), ty).is_some() {
            return Err(invalid(format!(
                "type '{full_name}' is defined more than once"
            )));
        }
        Ok(())
    }

    fn declare_enum(
        &mut self,
        d: &EnumDescriptorProto,
        parent: Option<usize>,
        scope: &str,
    ) -> Result<usize, SchemaError> {
        if self.enums.len() >= MAX_ENUMS {
            return Err(invalid(format!(
                "the schema has more than {MAX_ENUMS} enums"
            )));
        }
        let name = d.name().to_string();
        let full_name = join(scope, &name);
        if d.value.is_empty() {
            return Err(invalid(format!("enum '{name}' has no values")));
        }
        if d.value.len() > MAX_ENUM_VALUES_PER_ENUM {
            return Err(invalid(format!(
                "enum '{name}' has more than {MAX_ENUM_VALUES_PER_ENUM} values"
            )));
        }
        self.enum_values += d.value.len();
        if self.enum_values > MAX_ENUM_VALUES {
            return Err(invalid(format!(
                "the schema has more than {MAX_ENUM_VALUES} enum values"
            )));
        }
        let allow_alias = d.options.allow_alias();
        check_no_custom_options(d.options.uninterpreted_option.len())?;
        let mut values = Vec::with_capacity(d.value.len());
        let mut seen_names = HashSet::new();
        let mut numbers = BTreeSet::new();
        for v in &d.value {
            check_no_custom_options(v.options.uninterpreted_option.len())?;
            if !seen_names.insert(v.name().to_string()) {
                return Err(invalid(format!(
                    "enum '{name}' lists value '{}' more than once",
                    v.name()
                )));
            }
            if !numbers.insert(v.number()) && !allow_alias {
                return Err(invalid(format!(
                    "enum '{name}' gives two values the number {} without allow_alias",
                    v.number()
                )));
            }
            values.push((v.name().to_string(), v.number()));
        }
        if self.proto3 && values[0].1 != 0 {
            return Err(invalid(format!(
                "the first value of the proto3 enum '{name}' must be zero"
            )));
        }
        let id = self.enums.len();
        self.register(full_name.clone(), Ty::Enum(id))?;
        self.enums.push(EnumDef {
            name,
            full_name,
            parent,
            values,
            numbers: numbers.into_iter().collect(),
            closed: !self.proto3,
            allow_alias,
        });
        Ok(id)
    }

    fn declare_message(
        &mut self,
        d: &'a DescriptorProto,
        parent: Option<usize>,
        scope: &str,
        depth: usize,
    ) -> Result<usize, SchemaError> {
        if depth > MAX_MESSAGE_NESTING {
            return Err(invalid(format!(
                "messages are nested deeper than {MAX_MESSAGE_NESTING} levels"
            )));
        }
        if self.msgs.len() >= MAX_MESSAGES {
            return Err(invalid(format!(
                "the schema has more than {MAX_MESSAGES} messages (a map field counts as one)"
            )));
        }
        if !d.extension_range.is_empty() || !d.extension.is_empty() {
            return Err(invalid("extension ranges are not supported"));
        }
        if d.options.message_set_wire_format() {
            return Err(invalid("message_set_wire_format is not supported"));
        }
        check_no_custom_options(d.options.uninterpreted_option.len())?;
        let name = d.name().to_string();
        let full_name = join(scope, &name);
        let id = self.msgs.len();
        self.register(full_name.clone(), Ty::Message(id))?;
        self.msgs.push(Msg {
            name,
            full_name: full_name.clone(),
            parent,
            map_entry: d.options.map_entry(),
            ..Msg::default()
        });
        for nested in &d.nested_type {
            let child = self.declare_message(nested, Some(id), &full_name, depth + 1)?;
            if !self.msgs[child].map_entry {
                self.msgs[id].nested.push(child);
            }
        }
        for e in &d.enum_type {
            let enum_id = self.declare_enum(e, Some(id), &full_name)?;
            self.msgs[id].enums.push(enum_id);
        }
        self.pending.push((id, d));
        Ok(id)
    }

    fn resolve(&self, type_name: &str) -> Result<Ty, SchemaError> {
        self.ids
            .get(type_name.trim_start_matches('.'))
            .copied()
            .ok_or_else(|| invalid(format!("type '{type_name}' is not defined in the schema")))
    }

    fn fill_message(&mut self, id: usize, d: &DescriptorProto) -> Result<(), SchemaError> {
        let name = self.msgs[id].name.clone();
        if d.field.len() > MAX_FIELDS_PER_MESSAGE {
            return Err(invalid(format!(
                "message '{name}' has more than {MAX_FIELDS_PER_MESSAGE} fields"
            )));
        }
        self.fields += d.field.len();
        if self.fields > MAX_FIELDS {
            return Err(invalid(format!(
                "the schema has more than {MAX_FIELDS} fields"
            )));
        }
        if d.reserved_range.len() > MAX_RESERVED_RANGES {
            return Err(invalid(format!(
                "message '{name}' reserves more than {MAX_RESERVED_RANGES} ranges"
            )));
        }
        let mut reserved = Vec::with_capacity(d.reserved_range.len());
        for r in &d.reserved_range {
            // The descriptor's end is exclusive.
            let (first, last) = (i64::from(r.start()), i64::from(r.end()) - 1);
            if first < 1 || last < first || last > i64::from(MAX_FIELD_NUMBER) {
                return Err(invalid(format!(
                    "message '{name}' reserves numbers outside 1 to {MAX_FIELD_NUMBER}"
                )));
            }
            reserved.push((first as u32, last as u32));
        }
        let reserved = merge_ranges(reserved);
        let reserved_names: HashSet<&str> = d.reserved_name.iter().map(String::as_str).collect();
        let mut fields = Vec::with_capacity(d.field.len());
        let mut oneof_map: HashMap<i32, usize> = HashMap::new();
        let mut oneofs: Vec<String> = Vec::new();
        let mut names = HashSet::new();
        let mut numbers = HashSet::new();
        let mut required_bits = 0u32;
        let mut required_mask = 0u64;
        for f in &d.field {
            check_no_custom_options(f.options.uninterpreted_option.len())?;
            if f.type_() == Type::TYPE_GROUP {
                return Err(invalid("groups are not supported"));
            }
            if f.has_extendee() {
                return Err(invalid("extend is not supported"));
            }
            let number = f.number();
            if number < 1
                || number as u32 > MAX_FIELD_NUMBER
                || RESERVED_NUMBERS.contains(&(number as u32))
            {
                return Err(invalid(format!(
                    "field '{}' of message '{name}' has the number {number}, which is not in 1 to {MAX_FIELD_NUMBER} outside {}-{}",
                    f.name(),
                    RESERVED_NUMBERS.start(),
                    RESERVED_NUMBERS.end()
                )));
            }
            let number = number as u32;
            if !names.insert(f.name().to_string()) {
                return Err(invalid(format!(
                    "message '{name}' has two fields named '{}'",
                    f.name()
                )));
            }
            if !numbers.insert(number) {
                return Err(invalid(format!(
                    "message '{name}' has two fields numbered {number}"
                )));
            }
            if in_ranges(&reserved, number) || reserved_names.contains(f.name()) {
                return Err(invalid(format!(
                    "field '{}' of message '{name}' uses a reserved number or name",
                    f.name()
                )));
            }
            let cardinality = match f.label() {
                Label::LABEL_REQUIRED => Cardinality::Required,
                Label::LABEL_REPEATED => Cardinality::Repeated,
                Label::LABEL_OPTIONAL => Cardinality::Optional,
            };
            if cardinality == Cardinality::Required && self.proto3 {
                return Err(invalid("required fields do not exist in proto3"));
            }
            let ty = match Scalar::from_type(f.type_()) {
                Some(s) => Ty::Scalar(s),
                None => self.resolve(f.type_name())?,
            };
            let referenced_entry = matches!(ty, Ty::Message(m) if self.msgs[m].map_entry);
            if referenced_entry && cardinality != Cardinality::Repeated {
                return Err(invalid(format!(
                    "field '{}' of message '{name}' uses a map entry type as a plain field",
                    f.name()
                )));
            }
            let required_bit = if cardinality == Cardinality::Required {
                if required_bits as usize >= MAX_REQUIRED_FIELDS {
                    return Err(invalid(format!(
                        "message '{name}' has more than {MAX_REQUIRED_FIELDS} required fields"
                    )));
                }
                let bit = required_bits;
                required_bits += 1;
                required_mask |= 1u64 << bit;
                Some(bit)
            } else {
                None
            };
            let explicit_optional = f.proto3_optional();
            let oneof = if f.has_oneof_index() && !explicit_optional {
                let declared = f.oneof_index();
                Some(*oneof_map.entry(declared).or_insert_with(|| {
                    oneofs.push(
                        d.oneof_decl
                            .get(declared as usize)
                            .map_or_else(String::new, |o| o.name().to_string()),
                    );
                    oneofs.len() - 1
                }))
            } else {
                None
            };
            fields.push(Field {
                name: f.name().to_string(),
                number,
                cardinality,
                ty,
                oneof,
                explicit_optional,
                required_bit,
            });
        }
        if self.msgs[id].map_entry {
            let ok = fields.len() == 2
                && fields[0].number == 1
                && fields[1].number == 2
                && fields
                    .iter()
                    .all(|f| f.cardinality == Cardinality::Optional)
                && matches!(fields[0].ty, Ty::Scalar(s) if s.is_map_key())
                && !matches!(fields[1].ty, Ty::Message(m) if self.msgs[m].map_entry);
            if !ok {
                return Err(invalid(format!(
                    "map field '{}' has a key that is not an integer, bool or string, or a value that is a map",
                    name.strip_suffix("Entry").unwrap_or(&name)
                )));
            }
        }
        let mut by_number: Vec<(u32, usize)> = fields
            .iter()
            .enumerate()
            .map(|(i, f)| (f.number, i))
            .collect();
        by_number.sort_unstable();
        let msg = &mut self.msgs[id];
        msg.fields = fields;
        msg.by_number = by_number;
        msg.oneofs = oneofs;
        msg.reserved = reserved;
        msg.required_mask = required_mask;
        Ok(())
    }
}

fn lower(file: &FileDescriptorProto, designated: Option<&str>) -> Result<Compiled, SchemaError> {
    if !file.dependency.is_empty() {
        return Err(invalid(
            "import is not supported: a schema must be one self-contained file",
        ));
    }
    if !file.service.is_empty() {
        return Err(invalid("service is not supported"));
    }
    if !file.extension.is_empty() {
        return Err(invalid("extend is not supported"));
    }
    check_no_custom_options(file.options.uninterpreted_option.len())?;
    let proto3 = match file.syntax() {
        "proto3" => true,
        "" | "proto2" => false,
        _ => return Err(invalid("syntax must be \"proto2\" or \"proto3\"")),
    };
    let package = file.package().to_string();
    let mut lowering = Lowering {
        msgs: Vec::new(),
        enums: Vec::new(),
        ids: HashMap::new(),
        pending: Vec::new(),
        proto3,
        fields: 0,
        enum_values: 0,
    };
    let mut top_messages = Vec::new();
    for m in &file.message_type {
        top_messages.push(lowering.declare_message(m, None, &package, 1)?);
    }
    let mut top_enums = Vec::new();
    for e in &file.enum_type {
        top_enums.push(lowering.declare_enum(e, None, &package)?);
    }
    let pending = std::mem::take(&mut lowering.pending);
    for (id, d) in pending {
        lowering.fill_message(id, d)?;
    }
    let root = match designated {
        Some(name) => *top_messages
            .iter()
            .find(|&&m| lowering.msgs[m].name == name)
            .ok_or_else(|| {
                invalid(format!(
                    "the designated message '{name}' is not a top-level message of the schema"
                ))
            })?,
        None => *top_messages
            .first()
            .ok_or_else(|| invalid("the schema declares no message"))?,
    };
    Ok(Compiled {
        msgs: lowering.msgs,
        enums: lowering.enums,
        root,
        top_messages,
        top_enums,
        package,
        proto3,
    })
}

fn parse_text(schema_text: &str) -> Result<Compiled, SchemaError> {
    if schema_text.trim().is_empty() {
        return Err(invalid("schema text is empty"));
    }
    if schema_text.len() > MAX_SCHEMA_TEXT_BYTES {
        return Err(invalid(format!(
            "schema text is {} bytes; the limit is {MAX_SCHEMA_TEXT_BYTES}",
            schema_text.len()
        )));
    }
    let scan = prescan(schema_text)?;
    let file = run_parser(schema_text)?;
    lower(&file, scan.designated.as_deref())
}

// =============================================================================
// Validation
// =============================================================================

enum Fail {
    Violation(&'static str),
    Limit(&'static str),
}

impl From<LimitExceeded> for Fail {
    fn from(_: LimitExceeded) -> Fail {
        Fail::Limit(VALIDATION_BUDGET)
    }
}

type Walk<T> = Result<T, Fail>;

struct Frame {
    msg: usize,
    /// Offset of the first byte after this message.
    end: usize,
    /// Field of the parent that holds this message.
    via: usize,
    seen: u64,
}

struct Walker<'a> {
    c: &'a Compiled,
    data: &'a [u8],
    pos: usize,
    budget: &'a mut Budget,
    stack: Vec<Frame>,
    /// Field of the innermost message the failure is about.
    at: Option<usize>,
}

impl Walker<'_> {
    /// A varint of at most ten bytes that ends before `end`.
    fn varint(&mut self, end: usize) -> Walk<u64> {
        let mut value = 0u64;
        for i in 0..10 {
            if self.pos >= end {
                return Err(Fail::Violation(TRUNCATED));
            }
            let byte = self.data[self.pos];
            self.pos += 1;
            // The tenth byte may only carry bit 63.
            if i == 9 && byte > 1 {
                return Err(Fail::Violation(INT_TOO_LONG));
            }
            value |= u64::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(Fail::Violation(INT_TOO_LONG))
    }

    fn fixed(&mut self, size: usize, end: usize) -> Walk<()> {
        if size > end - self.pos {
            return Err(Fail::Violation(TRUNCATED));
        }
        self.pos += size;
        Ok(())
    }

    /// A length that fits in what is left of the enclosing message.
    fn length(&mut self, end: usize) -> Walk<usize> {
        let len = self.varint(end)?;
        if len > (end - self.pos) as u64 {
            return Err(Fail::Violation(LENGTH_EXCEEDS));
        }
        Ok(len as usize)
    }

    fn check_varint(&self, ty: Ty, v: u64) -> Walk<()> {
        let fits = match ty {
            Ty::Scalar(Scalar::Int32) => i32::try_from(v as i64).is_ok(),
            Ty::Scalar(Scalar::Uint32 | Scalar::Sint32) => v <= u64::from(u32::MAX),
            Ty::Scalar(Scalar::Bool) => {
                if v > 1 {
                    return Err(Fail::Violation(BAD_BOOLEAN));
                }
                true
            }
            Ty::Enum(e) => match i32::try_from(v as i64) {
                Ok(n) => {
                    let def = &self.c.enums[e];
                    if def.closed && def.numbers.binary_search(&n).is_err() {
                        return Err(Fail::Violation(ENUM_UNDECLARED));
                    }
                    true
                }
                Err(_) => false,
            },
            _ => true,
        };
        if fits {
            Ok(())
        } else {
            Err(Fail::Violation(VALUE_RANGE))
        }
    }

    /// Skips a field the schema does not declare, checking only its framing.
    fn skip(&mut self, wire: u8, end: usize) -> Walk<()> {
        match wire {
            0 => {
                self.varint(end)?;
            }
            1 => self.fixed(8, end)?,
            5 => self.fixed(4, end)?,
            _ => {
                let len = self.length(end)?;
                self.budget.charge(len as u64 / 32)?;
                self.pos += len;
            }
        }
        Ok(())
    }

    fn run(&mut self) -> Walk<()> {
        self.stack.push(Frame {
            msg: self.c.root,
            end: self.data.len(),
            via: usize::MAX,
            seen: 0,
        });
        while let Some(top) = self.stack.last() {
            let (msg, end) = (top.msg, top.end);
            self.at = None;
            if self.pos == end {
                self.close()?;
                continue;
            }
            self.budget.charge(1)?;
            let tag = self.varint(end)?;
            let number = tag >> 3;
            if number == 0 || number > u64::from(MAX_FIELD_NUMBER) {
                return Err(Fail::Violation(FIELD_NUMBER));
            }
            if RESERVED_NUMBERS.contains(&(number as u32)) {
                return Err(Fail::Violation(FIELD_RESERVED));
            }
            let wire = (tag & 7) as u8;
            match wire {
                3 | 4 => return Err(Fail::Violation(GROUP_WIRE)),
                6 | 7 => return Err(Fail::Violation(BAD_WIRE)),
                _ => {}
            }
            match self.c.msgs[msg].field_by_number(number as u32) {
                None => self.skip(wire, end)?,
                Some(index) => {
                    self.at = Some(index);
                    self.field(msg, index, wire, end)?;
                }
            }
        }
        Ok(())
    }

    /// Ends the innermost message: its `required` fields must all have come.
    fn close(&mut self) -> Walk<()> {
        let frame = self.stack.last().expect("a frame to close");
        let msg = &self.c.msgs[frame.msg];
        let missing = msg.required_mask & !frame.seen;
        if missing != 0 {
            let bit = missing.trailing_zeros();
            self.at = msg.fields.iter().position(|f| f.required_bit == Some(bit));
            return Err(Fail::Violation(REQUIRED_MISSING));
        }
        self.stack.pop();
        Ok(())
    }

    fn field(&mut self, msg: usize, index: usize, wire: u8, end: usize) -> Walk<()> {
        let c = self.c;
        let field = &c.msgs[msg].fields[index];
        let ty = field.ty;
        if let Some(bit) = field.required_bit {
            self.stack.last_mut().expect("a frame").seen |= 1u64 << bit;
        }
        if wire == wire_type(ty) {
            return match ty {
                Ty::Scalar(Scalar::String) => {
                    let len = self.length(end)?;
                    self.budget.charge(len as u64 / 16)?;
                    std::str::from_utf8(&self.data[self.pos..self.pos + len])
                        .map_err(|_| Fail::Violation(BAD_UTF8))?;
                    self.pos += len;
                    Ok(())
                }
                Ty::Scalar(Scalar::Bytes) => {
                    let len = self.length(end)?;
                    self.budget.charge(len as u64 / 32)?;
                    self.pos += len;
                    Ok(())
                }
                Ty::Message(child) => {
                    let len = self.length(end)?;
                    if self.stack.len() >= MAX_VALUE_DEPTH {
                        return Err(Fail::Limit(TOO_DEEP));
                    }
                    self.stack.push(Frame {
                        msg: child,
                        end: self.pos + len,
                        via: index,
                        seen: 0,
                    });
                    Ok(())
                }
                _ => match wire {
                    1 => self.fixed(8, end),
                    5 => self.fixed(4, end),
                    _ => {
                        let v = self.varint(end)?;
                        self.check_varint(ty, v)
                    }
                },
            };
        }
        if wire == 2 && field.cardinality == Cardinality::Repeated && wire_type(ty) != 2 {
            return self.packed(ty, end);
        }
        Err(Fail::Violation(WIRE_MISMATCH))
    }

    fn packed(&mut self, ty: Ty, end: usize) -> Walk<()> {
        let len = self.length(end)?;
        // One unit per byte, taken before the run is iterated.
        self.budget.charge(len as u64)?;
        let stop = self.pos + len;
        match wire_type(ty) {
            1 | 5 => {
                let size = if wire_type(ty) == 1 { 8 } else { 4 };
                if len % size != 0 {
                    return Err(Fail::Violation(PACKED_SIZE));
                }
                self.pos = stop;
            }
            _ => {
                while self.pos < stop {
                    let v = self.varint(stop)?;
                    self.check_varint(ty, v)?;
                }
            }
        }
        Ok(())
    }

    fn render_path(&self) -> String {
        let mut out = String::new();
        for k in 1..self.stack.len() {
            out.push('/');
            out.push_str(&self.c.msgs[self.stack[k - 1].msg].fields[self.stack[k].via].name);
        }
        if let (Some(frame), Some(at)) = (self.stack.last(), self.at) {
            out.push('/');
            out.push_str(&self.c.msgs[frame.msg].fields[at].name);
        }
        if out.is_empty() {
            out.push_str("<root>");
        }
        out
    }
}

fn validate_with(c: &Compiled, payload: &[u8], budget: &mut Budget) -> Result<(), SchemaError> {
    let mut walker = Walker {
        c,
        data: payload,
        pos: 0,
        budget,
        stack: Vec::new(),
        at: None,
    };
    match walker.run() {
        Ok(()) => Ok(()),
        Err(Fail::Violation(msg)) => Err(SchemaError::Violation(format!(
            "{}: {msg}",
            shorten_path(&walker.render_path())
        ))),
        Err(Fail::Limit(msg)) => Err(SchemaError::LimitExceeded(format!(
            "{}: {msg}",
            shorten_path(&walker.render_path())
        ))),
    }
}

// =============================================================================
// Compatibility
// =============================================================================

enum Stop {
    No(String),
    Complex,
}

impl From<LimitExceeded> for Stop {
    fn from(_: LimitExceeded) -> Stop {
        Stop::Complex
    }
}

struct Compat<'a> {
    writer: &'a Compiled,
    reader: &'a Compiled,
    /// Which side is the OLD schema (backward: the writer, forward: the reader).
    old_is_writer: bool,
    budget: Budget,
    visited: HashSet<(usize, usize)>,
}

impl Compat<'_> {
    fn labels(&self) -> (&'static str, &'static str) {
        if self.old_is_writer {
            ("old", "new")
        } else {
            ("new", "old")
        }
    }

    fn messages(
        &mut self,
        w: usize,
        r: usize,
        path: &mut String,
        depth: usize,
    ) -> Result<(), Stop> {
        if !self.visited.insert((w, r)) {
            return Ok(());
        }
        if depth > MAX_COMPAT_DEPTH {
            return Err(Stop::Complex);
        }
        let (wl, rl) = self.labels();
        let (writer, reader) = (self.writer, self.reader);
        let (wm, rm) = (&writer.msgs[w], &reader.msgs[r]);
        let (old_msg, new_msg) = if self.old_is_writer {
            (wm, rm)
        } else {
            (rm, wm)
        };
        for (number, index) in &new_msg.by_number {
            self.budget.charge(1)?;
            if old_msg.is_reserved(*number) {
                let field = &new_msg.fields[*index];
                return Err(Stop::No(format!(
                    "{path}/{}: field number {number} is reserved by the old schema and cannot be used by the new one",
                    field.name
                )));
            }
        }
        for (number, r_index) in &rm.by_number {
            self.budget.charge(1)?;
            let rf = &rm.fields[*r_index];
            let Some(w_index) = wm.field_by_number(*number) else {
                if rf.cardinality == Cardinality::Required {
                    return Err(Stop::No(format!(
                        "{path}/{}: the {rl} schema requires field '{}' (number {number}) of message '{}', which the {wl} schema does not always write",
                        rf.name, rf.name, rm.name
                    )));
                }
                continue;
            };
            let wf = &wm.fields[w_index];
            if rf.cardinality == Cardinality::Required && wf.cardinality != Cardinality::Required {
                return Err(Stop::No(format!(
                    "{path}/{}: the {rl} schema requires field '{}' (number {number}) of message '{}', which the {wl} schema does not always write",
                    rf.name, rf.name, rm.name
                )));
            }
            let same_shape = (wf.cardinality == Cardinality::Repeated)
                == (rf.cardinality == Cardinality::Repeated);
            let same_type = match (wf.ty, rf.ty) {
                (Ty::Scalar(a), Ty::Scalar(b)) => a == b,
                (Ty::Enum(_), Ty::Enum(_)) => true,
                (Ty::Message(a), Ty::Message(b)) => {
                    writer.msgs[a].map_entry == reader.msgs[b].map_entry
                }
                _ => false,
            };
            if !same_shape || !same_type {
                return Err(Stop::No(format!(
                    "{path}/{}: {} written by the {wl} schema cannot be read as {} by the {rl} schema",
                    rf.name,
                    writer.field_type_label(wf),
                    reader.field_type_label(rf),
                )));
            }
            match (wf.ty, rf.ty) {
                (Ty::Enum(we), Ty::Enum(re)) => {
                    self.enums(we, re, &format!("{path}/{}", rf.name))?;
                }
                (Ty::Message(wc), Ty::Message(rc)) => {
                    let keep = path.len();
                    path.push('/');
                    path.push_str(&rf.name);
                    let verdict = self.messages(wc, rc, path, depth + 1);
                    path.truncate(keep);
                    verdict?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn enums(&mut self, w: usize, r: usize, path: &str) -> Result<(), Stop> {
        let (wl, rl) = self.labels();
        let (writer, reader) = (self.writer, self.reader);
        let (we, re) = (&writer.enums[w], &reader.enums[r]);
        if !re.closed {
            return Ok(());
        }
        if !we.closed {
            return Err(Stop::No(format!(
                "{path}: enum '{}' of the {wl} schema accepts any number, but the enum of the {rl} schema is closed",
                re.name
            )));
        }
        self.budget.charge(we.numbers.len() as u64)?;
        match we.numbers.iter().find(|n| re.numbers.binary_search(n).is_err()) {
            Some(value) => Err(Stop::No(format!(
                "{path}: enum '{}': value {value} of the {wl} schema is unknown to the {rl} schema, whose enum is closed",
                re.name
            ))),
            None => Ok(()),
        }
    }
}

/// `Ok` when every message written under `writer` can be read under `reader`,
/// within `limit` units of work.
fn reads(
    writer: &Compiled,
    reader: &Compiled,
    old_is_writer: bool,
    limit: u64,
) -> Result<(), Stop> {
    let mut compat = Compat {
        writer,
        reader,
        old_is_writer,
        budget: Budget::new(limit),
        visited: HashSet::new(),
    };
    compat.messages(writer.root, reader.root, &mut String::new(), 0)
}

// =============================================================================
// Projection
// =============================================================================

/// Messages written in full, enums kept, and messages that only contain kept
/// types (written without fields).
struct Keep {
    full: HashSet<usize>,
    enums: HashSet<usize>,
    containers: HashSet<usize>,
}

fn collect_keep(c: &Compiled, allowed: &BTreeSet<String>) -> Keep {
    let mut keep = Keep {
        full: HashSet::from([c.root]),
        enums: HashSet::new(),
        containers: HashSet::new(),
    };
    let mut work = vec![c.root];
    while let Some(m) = work.pop() {
        for field in &c.msgs[m].fields {
            if m == c.root && !allowed.contains(&field.name) {
                continue;
            }
            match field.ty {
                Ty::Scalar(_) => {}
                Ty::Enum(e) => {
                    keep.enums.insert(e);
                }
                // A map's entry type is written as `map<K, V>`, so it is the
                // types of its key and value that are reached.
                Ty::Message(entry) if c.msgs[entry].map_entry => {
                    for entry_field in &c.msgs[entry].fields {
                        match entry_field.ty {
                            Ty::Scalar(_) => {}
                            Ty::Enum(e) => {
                                keep.enums.insert(e);
                            }
                            Ty::Message(value) => {
                                if keep.full.insert(value) {
                                    work.push(value);
                                }
                            }
                        }
                    }
                }
                Ty::Message(next) => {
                    if keep.full.insert(next) {
                        work.push(next);
                    }
                }
            }
        }
    }
    let mut parents: Vec<Option<usize>> = keep.full.iter().map(|&m| c.msgs[m].parent).collect();
    parents.extend(keep.enums.iter().map(|&e| c.enums[e].parent));
    for parent in parents {
        let mut at = parent;
        while let Some(p) = at {
            if !keep.full.contains(&p) {
                keep.containers.insert(p);
            }
            at = c.msgs[p].parent;
        }
    }
    keep
}

struct Printer<'a> {
    c: &'a Compiled,
    keep: &'a Keep,
    allowed: &'a BTreeSet<String>,
    out: String,
}

impl Printer<'_> {
    fn type_ref(&self, ty: Ty) -> String {
        match ty {
            Ty::Scalar(s) => s.name().to_string(),
            Ty::Enum(e) => format!(".{}", self.c.enums[e].full_name),
            Ty::Message(m) => format!(".{}", self.c.msgs[m].full_name),
        }
    }

    fn enum_def(&mut self, e: usize, indent: usize) {
        let def = &self.c.enums[e];
        let pad = "  ".repeat(indent);
        let _ = writeln!(self.out, "{pad}enum {} {{", def.name);
        if def.allow_alias {
            let _ = writeln!(self.out, "{pad}  option allow_alias = true;");
        }
        for (name, number) in &def.values {
            let _ = writeln!(self.out, "{pad}  {name} = {number};");
        }
        let _ = writeln!(self.out, "{pad}}}");
    }

    fn field(&mut self, field: &Field, indent: usize, in_oneof: bool) {
        let pad = "  ".repeat(indent);
        let label = if in_oneof {
            ""
        } else if field.cardinality == Cardinality::Repeated && !self.c.is_map(field) {
            "repeated "
        } else if field.cardinality == Cardinality::Required {
            "required "
        } else if field.explicit_optional || (!self.c.proto3 && !self.c.is_map(field)) {
            "optional "
        } else {
            ""
        };
        let ty = match field.ty {
            Ty::Message(m) if self.c.msgs[m].map_entry => {
                let entry = &self.c.msgs[m];
                format!(
                    "map<{}, {}>",
                    self.type_ref(entry.fields[0].ty),
                    self.type_ref(entry.fields[1].ty)
                )
            }
            other => self.type_ref(other),
        };
        let _ = writeln!(
            self.out,
            "{pad}{label}{ty} {} = {};",
            field.name, field.number
        );
    }

    fn message(&mut self, m: usize, indent: usize) {
        let pad = "  ".repeat(indent);
        let msg = &self.c.msgs[m];
        let _ = writeln!(self.out, "{pad}message {} {{", msg.name);
        for &nested in &msg.nested {
            if self.keep.full.contains(&nested) || self.keep.containers.contains(&nested) {
                self.message(nested, indent + 1);
            }
        }
        for &e in &msg.enums {
            if self.keep.enums.contains(&e) {
                self.enum_def(e, indent + 1);
            }
        }
        if self.keep.full.contains(&m) {
            let kept = |f: &Field| m != self.c.root || self.allowed.contains(&f.name);
            let mut printed_oneofs = HashSet::new();
            for field in msg.fields.iter().filter(|f| kept(f)) {
                match field.oneof {
                    None => self.field(field, indent + 1, false),
                    Some(o) if printed_oneofs.insert(o) => {
                        let _ = writeln!(self.out, "{pad}  oneof {} {{", msg.oneofs[o]);
                        for member in msg.fields.iter().filter(|f| kept(f) && f.oneof == Some(o)) {
                            self.field(member, indent + 2, true);
                        }
                        let _ = writeln!(self.out, "{pad}  }}");
                    }
                    Some(_) => {}
                }
            }
        }
        let _ = writeln!(self.out, "{pad}}}");
    }
}

fn derive(c: &Compiled, allowed: &BTreeSet<String>) -> String {
    let keep = collect_keep(c, allowed);
    let mut printer = Printer {
        c,
        keep: &keep,
        allowed,
        out: String::new(),
    };
    let _ = writeln!(
        printer.out,
        "syntax = \"{}\";",
        if c.proto3 { "proto3" } else { "proto2" }
    );
    if !c.package.is_empty() {
        let _ = writeln!(printer.out, "package {};", c.package);
    }
    printer.out.push('\n');
    printer.message(c.root, 0);
    for &m in &c.top_messages {
        if m != c.root && (keep.full.contains(&m) || keep.containers.contains(&m)) {
            printer.out.push('\n');
            printer.message(m, 0);
        }
    }
    for &e in &c.top_enums {
        if keep.enums.contains(&e) {
            printer.out.push('\n');
            printer.enum_def(e, 0);
        }
    }
    printer.out
}

// =============================================================================
// SchemaKindOps
// =============================================================================

pub struct ProtobufOps;

impl SchemaKindOps for ProtobufOps {
    fn compile(&self, schema_text: &str) -> Result<CompiledSchema, SchemaError> {
        parse_text(schema_text).map(CompiledSchema::Protobuf)
    }

    fn validate_metered(
        &self,
        compiled: &CompiledSchema,
        payload: &[u8],
        shared: &mut ValidationBudget,
    ) -> Result<(), SchemaError> {
        let CompiledSchema::Protobuf(c) = compiled else {
            return Err(invalid(
                "validate called with a non-protobuf compiled schema",
            ));
        };
        let mut budget = Budget::new(shared.allowance(MAX_VALIDATION_UNITS));
        let verdict = validate_with(c, payload, &mut budget);
        shared.spend(budget.used);
        verdict
    }

    fn derive_subschema(
        &self,
        schema_text: &str,
        allowed: &BTreeSet<String>,
    ) -> Result<String, SchemaError> {
        let compiled = parse_text(schema_text)?;
        let derived = derive(&compiled, allowed);
        // The projection must itself be a registrable schema.
        parse_text(&derived)?;
        Ok(derived)
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
        let old = parse_text(old_schema_text)?;
        let new = parse_text(new_schema_text)?;
        let check =
            |writer: &Compiled, reader: &Compiled, old_is_writer: bool, lead: &str| match reads(
                writer,
                reader,
                old_is_writer,
                MAX_COMPAT_WORK,
            ) {
                Ok(()) => Ok(()),
                Err(Stop::No(why)) => Err(SchemaError::Incompatible(format!("{lead} - {why}"))),
                Err(Stop::Complex) => Err(SchemaError::LimitExceeded(TOO_COMPLEX.to_string())),
            };
        let backward = || {
            check(
                &old,
                &new,
                true,
                "backward: data written under the old schema may not be readable with the new one",
            )
        };
        let forward = || {
            check(
                &new,
                &old,
                false,
                "forward: data written under the new schema may not be readable with the old one",
            )
        };
        match mode {
            Compatibility::Backward => backward(),
            Compatibility::Forward => forward(),
            Compatibility::Full => backward().and_then(|()| forward()),
            Compatibility::None => Ok(()),
        }
    }
}

pub(super) static PROTOBUF_OPS: ProtobufOps = ProtobufOps;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::schema_registry::fixtures;
    use std::time::{Duration, Instant};

    // -------------------------------------------------------------------------
    // Helpers
    // -------------------------------------------------------------------------

    fn varint(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    fn tag(number: u32, wire: u8) -> Vec<u8> {
        varint((u64::from(number) << 3) | u64::from(wire))
    }

    fn ld(number: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = tag(number, 2);
        out.extend(varint(payload.len() as u64));
        out.extend_from_slice(payload);
        out
    }

    fn vf(number: u32, value: u64) -> Vec<u8> {
        let mut out = tag(number, 0);
        out.extend(varint(value));
        out
    }

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    fn compile(text: &str) -> CompiledSchema {
        PROTOBUF_OPS
            .compile(text)
            .unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    fn refusal(text: &str) -> String {
        match PROTOBUF_OPS.compile(text) {
            Err(SchemaError::Invalid(m)) => m,
            other => panic!("expected a refusal of {text:?}, got {other:?}"),
        }
    }

    fn check(compiled: &CompiledSchema, payload: &[u8]) -> Result<(), SchemaError> {
        PROTOBUF_OPS.validate(compiled, payload)
    }

    fn violation(compiled: &CompiledSchema, payload: &[u8]) -> String {
        match check(compiled, payload) {
            Err(SchemaError::Violation(m)) => m,
            other => panic!("expected a violation for {payload:?}, got {other:?}"),
        }
    }

    fn proto3(body: &str) -> String {
        format!("syntax = \"proto3\"; message M {{ {body} }}")
    }

    fn proto2(body: &str) -> String {
        format!("syntax = \"proto2\"; message M {{ {body} }}")
    }

    /// Runs `f` on a thread with a stack this small, so a recursion that the
    /// caps are supposed to prevent aborts the test process loudly.
    fn on_stack<T: Send + 'static>(kib: usize, f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(kib * 1024)
            .spawn(f)
            .unwrap()
            .join()
            .unwrap()
    }

    fn timed<T>(what: &str, limit: Duration, f: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let out = f();
        let took = start.elapsed();
        eprintln!("timing: {what}: {took:?}");
        assert!(took < limit, "{what} took {took:?}, limit {limit:?}");
        out
    }

    /// A tiny deterministic generator, so the fuzz tests replay.
    struct Xorshift(u64);

    impl Xorshift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    // -------------------------------------------------------------------------
    // Golden fixtures (produced by `protoc --encode`, or written by hand for
    // the malformed ones; `*.bin` is gitignored, hence `.datum`)
    // -------------------------------------------------------------------------

    fn golden(format: &str, schema_file: &str, message: &str) {
        let text = fixtures::read(format, schema_file);
        let compiled = compile(&text);
        let oracle = Oracle::new(&text, message);
        for case in fixtures::cases(format, "datum") {
            let got = check(&compiled, &case.payload);
            match &case.expect {
                None => {
                    assert_eq!(got, Ok(()), "{}", case.name);
                    assert!(
                        oracle.accepts(&case.payload),
                        "{}: the library decoder refuses a document we accept",
                        case.name
                    );
                }
                Some(expect) => {
                    assert!(
                        matches!(&got, Err(SchemaError::Violation(m)) if m.contains(expect.as_str())),
                        "{}: expected a violation containing {expect:?}, got {got:?}",
                        case.name
                    );
                }
            }
        }
    }

    #[test]
    fn golden_proto3_documents_validate_and_refusals_carry_their_phrase() {
        golden("protobuf", "order.proto", "Order");
    }

    #[test]
    fn golden_proto2_documents_validate_and_refusals_carry_their_phrase() {
        golden("protobuf/proto2", "legacy.proto", "Legacy");
    }

    // -------------------------------------------------------------------------
    // Schema text
    // -------------------------------------------------------------------------

    #[test]
    fn schema_text_refusals_are_stable() {
        // The dashboard translates these (`schema-windows.js`
        // `textRefusalReason`): rewording one turns a plain-language
        // refusal back into a generic one.
        let imports = "syntax = \"proto3\"; import \"google/protobuf/any.proto\"; message M {}";
        let mut misses = Vec::new();
        for (text, phrase) in [
            ("   ".to_string(), "schema text is empty"),
            ("syntax = \"proto3\";".to_string(), "the schema declares no message"),
            (imports.to_string(), "import is not supported"),
            (
                "syntax = \"proto3\"; message M {} service S { rpc A(M) returns (M); }".to_string(),
                "service is not supported",
            ),
            (
                "syntax = \"proto2\"; message A {} extend A { optional int32 x = 100; }".to_string(),
                "extend is not supported",
            ),
            (
                proto2("optional int32 a = 1; extensions 100 to 199;"),
                "extension ranges are not supported",
            ),
            (
                proto2("optional group G = 1 { optional int32 a = 1; }"),
                "groups are not supported",
            ),
            (
                "syntax = \"proto3\"; option (my.opt) = 1; message M {}".to_string(),
                "custom options are not supported",
            ),
            (
                proto3("int32 a = 1 [(my.opt) = 1];"),
                "custom options are not supported",
            ),
            (
                "edition = \"2023\"; message M {}".to_string(),
                "not a valid Protobuf schema",
            ),
            // The parser's own check (it words a repeated number like a repeated name).
            (
                proto3("int32 a = 1; int32 b = 1;"),
                "Non-unique field name: `b`",
            ),
            (
                proto3("int32 a = 1; int32 a = 2;"),
                "Non-unique field name: `a`",
            ),
            (
                proto3("int32 a = 0;"),
                "field 'a' of message 'M' has the number 0, which is not in 1 to 536870911 outside 19000-19999",
            ),
            (
                proto3("int32 a = 536870912;"),
                "has the number 536870912, which is not in 1 to 536870911 outside 19000-19999",
            ),
            (
                proto3("int32 a = -1;"),
                "not a valid Protobuf schema",
            ),
            (
                proto3("int32 a = 19000;"),
                "which is not in 1 to 536870911 outside 19000-19999",
            ),
            (
                proto3("reserved 5 to 9; int32 a = 7;"),
                "field 'a' of message 'M' uses a reserved number or name",
            ),
            (
                proto3("reserved \"a\"; int32 a = 7;"),
                "uses a reserved number or name",
            ),
            (
                proto3("Missing a = 1;"),
                "not a valid Protobuf schema",
            ),
            (
                "syntax = \"proto3\"; enum E { A = 1; } message M { E e = 1; }".to_string(),
                "the first value of the proto3 enum 'E' must be zero",
            ),
            (
                "syntax = \"proto3\"; enum E { A = 0; B = 0; } message M { E e = 1; }".to_string(),
                "gives two values the number 0 without allow_alias",
            ),
            (
                proto3("map<float, int32> m = 1;"),
                "has a key that is not an integer, bool or string",
            ),
            (
                proto3("map<bytes, int32> m = 1;"),
                "has a key that is not an integer, bool or string",
            ),
            (
                proto3("required int32 a = 1;"),
                "not a valid Protobuf schema",
            ),
            (
                proto3("option message_set_wire_format = true;"),
                "message_set_wire_format is not supported",
            ),
            (
                "syntax = \"proto3\"; message A {} message A {}".to_string(),
                "type 'A' is defined more than once",
            ),
            (
                "syntax = \"proto3\"; message M { } }".to_string(),
                "a closing bracket has no opening one",
            ),
            (
                "syntax = \"proto3\"; message M { int32 a = 1;".to_string(),
                "a bracket is never closed",
            ),
            (
                "syntax = \"proto3\"; /* message M {}".to_string(),
                "a comment is never closed",
            ),
            (
                "syntax = \"proto3\"; option java_package = \"x\nmessage M {}".to_string(),
                "a string is never closed on its line",
            ),
            (
                "syntax = \"proto3\"; message M { int32 a\u{a0}= 1; }".to_string(),
                "the schema contains a character a .proto file cannot",
            ),
            (
                format!("syntax = \"proto3\"; option java_package = \"{}\"; message M {{}}", "a".repeat(257)),
                "a string is longer than 256 bytes",
            ),
            (
                format!("syntax = \"proto3\"; message {} {{}}", "A".repeat(129)),
                "a name is longer than 128 characters",
            ),
            (
                format!("// {}\nsyntax = \"proto3\"; message M {{}}", "x".repeat(MAX_SCHEMA_TEXT_BYTES)),
                "the limit is 262144",
            ),
        ] {
            let got = match PROTOBUF_OPS.compile(&text) {
                Err(SchemaError::Invalid(m)) => m,
                other => format!("accepted: {}", format!("{other:?}").chars().take(40).collect::<String>()),
            };
            if !got.contains(phrase) {
                misses.push(format!("{got:?} should contain {phrase:?}"));
            }
        }
        assert_eq!(misses, Vec::<String>::new());
        // A field may be called like a keyword.
        compile(&proto3(
            "string import = 1; string service = 2; string message = 3; string extend = 4;",
        ));
    }

    #[test]
    fn the_staging_path_is_scrubbed_from_any_parser_message() {
        let staged = "/var/folders/xx/T/tf-proto-AbC123";
        let error =
            anyhow::anyhow!("protobuf path `x.proto` is not found in import path [\"{staged}\"]")
                .context("error in `schema.proto`");
        let got = parser_failure(&error, staged);
        assert_eq!(
            got,
            SchemaError::Invalid(
                "not a valid Protobuf schema: protobuf path `x.proto` is not found in import path [\"<schema>\"]"
                    .to_string()
            )
        );
        // The line comes from the located message in the chain, and a long message is cut.
        let located = anyhow::anyhow!("x".repeat(1000)).context("at 12:5: boom");
        let SchemaError::Invalid(text) = parser_failure(&located, staged) else {
            panic!()
        };
        assert!(
            text.starts_with("not a valid Protobuf schema (line 12): "),
            "{text}"
        );
        assert_eq!(
            text.chars().count(),
            "not a valid Protobuf schema (line 12): ".len() + MAX_PARSER_MESSAGE
        );
    }

    #[test]
    fn a_type_name_may_repeat_only_in_another_scope() {
        // The parser panics (an `unwrap` in its option resolver) when two
        // messages of one name differ; the pre-scan refuses them first.
        for text in [
            "syntax = \"proto3\"; message M { int32 a = 1; } message M { int32 b = 1; }",
            "syntax = \"proto3\"; message A { message B {} enum B { Z = 0; } }",
            "syntax = \"proto3\"; enum E { Z = 0; } message E {}",
            "syntax = \"proto3\"; message A { message B {} message B { int32 x = 1; } }",
        ] {
            assert!(
                refusal(text).starts_with("type '")
                    && refusal(text).ends_with("' is defined more than once"),
                "{text}: {}",
                refusal(text)
            );
        }
        compile(
            "syntax = \"proto3\"; message A { message B { int32 x = 1; } enum E { Z = 0; } } \
             message C { message B { string y = 1; } enum E { Z = 0; } } enum E { Z = 0; }",
        );
        // An enum value or field named like a keyword is not a declaration.
        compile("syntax = \"proto3\"; enum E { message = 0; enum = 1; service = 2; import = 3; extend = 4; } message M { E e = 1; }");
    }

    #[test]
    fn a_parser_failure_names_the_line_and_never_the_staging_path() {
        let got = refusal("syntax = \"proto3\";\n\nmessage M {\n  Missing a = 1;\n}\n");
        assert!(got.starts_with("not a valid Protobuf schema"), "{got}");
        assert!(
            !got.contains("schema.proto") && !got.contains("/tmp") && !got.contains("/var/"),
            "{got}"
        );
        assert!(got.len() < 300, "{got}");
        let got = refusal("syntax = \"proto3\";\nmessage M {\n  int32 a = ;\n}\n");
        assert!(got.contains("(line 3)"), "{got}");
    }

    #[test]
    fn the_designated_message_is_the_first_one_or_the_one_a_comment_names() {
        let body = "message Helper { int32 h = 1; }\nmessage Order { string o = 1; }\n";
        let first = compile(&format!("syntax = \"proto3\";\n{body}"));
        assert_eq!(check(&first, &vf(1, 5)), Ok(()));
        assert_eq!(
            violation(&first, &ld(1, b"x")),
            "/h: wire type does not match the type declared for the field"
        );
        let named = compile(&format!(
            "// tentabus:message Order\nsyntax = \"proto3\";\n{body}"
        ));
        assert_eq!(check(&named, &ld(1, b"x")), Ok(()));
        assert_eq!(
            violation(&named, &vf(1, 5)),
            "/o: wire type does not match the type declared for the field"
        );
        // The comment may stand anywhere, with any spacing.
        compile(&format!(
            "syntax = \"proto3\";\n{body}//    tentabus:message   Helper  \n"
        ));
        // A message in a block comment or a differently worded comment is not a designation.
        let ignored = compile(&format!("/* tentabus:message Order */\n// tentabus:messages Order\nsyntax = \"proto3\";\n{body}"));
        assert_eq!(check(&ignored, &vf(1, 5)), Ok(()));
        for (text, phrase) in [
            (format!("// tentabus:message Nope\nsyntax = \"proto3\";\n{body}"), "the designated message 'Nope' is not a top-level message"),
            (format!("// tentabus:message\nsyntax = \"proto3\";\n{body}"), "must be followed by the name of a message"),
            (format!("// tentabus:message 9x\nsyntax = \"proto3\";\n{body}"), "must be followed by the name of a message"),
            (format!("// tentabus:message Order\n// tentabus:message Helper\nsyntax = \"proto3\";\n{body}"), "appears more than once"),
            (
                "// tentabus:message Inner\nsyntax = \"proto3\"; message Outer { message Inner { int32 a = 1; } }".to_string(),
                "the designated message 'Inner' is not a top-level message",
            ),
        ] {
            let got = refusal(&text);
            assert!(got.contains(phrase), "{got:?} should contain {phrase:?}");
        }
    }

    #[test]
    fn options_that_do_not_change_the_wire_are_accepted() {
        compile(
            r#"syntax = "proto3";
            package a.b;
            option java_package = "x.y";
            option go_package = "x/y";
            option optimize_for = SPEED;
            option deprecated = true;
            message M {
              option deprecated = true;
              int32 a = 1 [deprecated = true, json_name = "alpha"];
              repeated int32 b = 2 [packed = false];
              reserved 4, 6 to 8;
              reserved "old";
              enum E { option allow_alias = true; Z = 0; Y = 0; X = 1 [deprecated = true]; }
              E e = 3;
            }"#,
        );
        compile(&proto2(
            "optional int32 a = 1 [default = 5]; optional string s = 2 [default = \"x\"];",
        ));
    }

    // -------------------------------------------------------------------------
    // Validation
    // -------------------------------------------------------------------------

    const ALL_SCALARS: &str = "int32 a = 1; int64 b = 2; uint32 c = 3; uint64 d = 4; sint32 e = 5; sint64 f = 6; \
                               bool g = 7; fixed32 h = 8; fixed64 i = 9; sfixed32 j = 10; sfixed64 k = 11; \
                               float l = 12; double m = 13; string n = 14; bytes o = 15;";

    /// The wire type every scalar field of `ALL_SCALARS` is written with.
    const NATURAL_WIRE: [(u32, u8); 15] = [
        (1, 0),
        (2, 0),
        (3, 0),
        (4, 0),
        (5, 0),
        (6, 0),
        (7, 0),
        (8, 5),
        (9, 1),
        (10, 5),
        (11, 1),
        (12, 5),
        (13, 1),
        (14, 2),
        (15, 2),
    ];

    fn generic_value(wire: u8) -> Vec<u8> {
        match wire {
            0 => vec![1],
            1 => vec![0; 8],
            2 => vec![0],
            _ => vec![0; 4],
        }
    }

    #[test]
    fn every_scalar_is_accepted_with_its_wire_type_and_refused_with_any_other() {
        let compiled = compile(&proto3(ALL_SCALARS));
        for (number, natural) in NATURAL_WIRE {
            for wire in [0u8, 1, 2, 5] {
                let payload = cat(&[&tag(number, wire), &generic_value(wire)]);
                let got = check(&compiled, &payload);
                if wire == natural {
                    assert_eq!(got, Ok(()), "field {number} wire {wire}");
                } else {
                    assert!(
                        matches!(&got, Err(SchemaError::Violation(m)) if m.ends_with("wire type does not match the type declared for the field")),
                        "field {number} wire {wire}: {got:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn integer_values_must_fit_their_declared_type() {
        let compiled = compile(&proto3(ALL_SCALARS));
        let negative = |v: i64| v as u64;
        for (number, value, fits) in [
            (1, 2147483647u64, true),
            (1, negative(-2147483648), true),
            (1, 2147483648, false),
            (1, negative(-2147483649), false),
            (2, u64::MAX, true),
            (3, 4294967295, true),
            (3, 4294967296, false),
            (3, negative(-1), false),
            (4, u64::MAX, true),
            (5, 4294967295, true),
            (5, 4294967296, false),
            (6, u64::MAX, true),
            (7, 0, true),
            (7, 1, true),
            (7, 2, false),
        ] {
            let got = check(&compiled, &vf(number, value));
            assert_eq!(got.is_ok(), fits, "field {number} value {value}: {got:?}");
            if !fits {
                let m = violation(&compiled, &vf(number, value));
                assert!(
                    m.ends_with("value does not fit the declared type")
                        || m.ends_with("boolean is neither 0 nor 1"),
                    "{m}"
                );
            }
        }
    }

    #[test]
    fn a_value_cut_short_by_its_message_is_a_violation() {
        let compiled = compile(&proto3(ALL_SCALARS));
        for (payload, expected) in [
            (vec![0x08], "/a: payload ends before the value is complete"),
            (
                vec![0x80],
                "<root>: payload ends before the value is complete",
            ),
            (
                vec![0x08, 0x80],
                "/a: payload ends before the value is complete",
            ),
            (
                cat(&[&tag(8, 5), &[0, 0, 0]]),
                "/h: payload ends before the value is complete",
            ),
            (
                cat(&[&tag(9, 1), &[0; 7]]),
                "/i: payload ends before the value is complete",
            ),
            (
                cat(&[&tag(14, 2), &[5, b'a']]),
                "/n: declared length exceeds the remaining payload",
            ),
            (
                cat(&[&tag(15, 2)]),
                "/o: payload ends before the value is complete",
            ),
            (
                cat(&[&tag(14, 2), &[0xff; 11]]),
                "/n: integer encoding is longer than 10 bytes or overflows 64 bits",
            ),
            (
                cat(&[
                    &tag(14, 2),
                    &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02],
                ]),
                "/n: integer encoding is longer than 10 bytes or overflows 64 bits",
            ),
            (
                cat(&[
                    &tag(14, 2),
                    &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
                ]),
                "/n: declared length exceeds the remaining payload",
            ),
        ] {
            assert_eq!(violation(&compiled, &payload), expected, "{payload:?}");
        }
    }

    #[test]
    fn a_string_must_be_utf8_and_bytes_need_not_be() {
        let compiled = compile(&proto3(ALL_SCALARS));
        assert_eq!(check(&compiled, &ld(14, "żółć 🎉".as_bytes())), Ok(()));
        assert_eq!(
            violation(&compiled, &ld(14, &[0xc3, 0x28])),
            "/n: string is not valid UTF-8"
        );
        assert_eq!(check(&compiled, &ld(15, &[0xff, 0xfe, 0x00])), Ok(()));
    }

    #[test]
    fn field_numbers_are_checked_before_the_schema_is_consulted() {
        let compiled = compile(&proto3("int32 a = 1;"));
        for (payload, expected) in [
            (vec![0x00, 0x01], "<root>: field number is out of range"),
            (tag(536_870_912, 0), "<root>: field number is out of range"),
            (
                cat(&[&varint(u64::MAX >> 1), &[0]]),
                "<root>: field number is out of range",
            ),
            (
                cat(&[&vf(1, 1), &tag(19_000, 0), &[1]]),
                "<root>: field number is in the range reserved for the protobuf implementation",
            ),
            (
                cat(&[&tag(19_999, 2), &[0]]),
                "<root>: field number is in the range reserved for the protobuf implementation",
            ),
            (
                cat(&[&tag(1, 3)]),
                "<root>: group wire types are not supported",
            ),
            (
                cat(&[&tag(7, 4)]),
                "<root>: group wire types are not supported",
            ),
            (cat(&[&tag(1, 6)]), "<root>: wire type is not valid"),
            (cat(&[&tag(99, 7)]), "<root>: wire type is not valid"),
        ] {
            assert_eq!(violation(&compiled, &payload), expected, "{payload:?}");
        }
        // The largest and the neighbours of the reserved range are fine (skipped).
        for number in [18_999u32, 20_000, 536_870_911] {
            assert_eq!(
                check(&compiled, &cat(&[&tag(number, 0), &[1]])),
                Ok(()),
                "{number}"
            );
        }
    }

    #[test]
    fn a_field_the_schema_does_not_declare_is_skipped_but_its_framing_is_checked() {
        let compiled = compile(&proto3("int32 a = 1;"));
        let unknown = cat(&[
            &vf(1, 5),
            &vf(50, 300),
            &ld(51, b"abc"),
            &cat(&[&tag(52, 5), &[1, 2, 3, 4]]),
            &cat(&[&tag(53, 1), &[0; 8]]),
        ]);
        assert_eq!(check(&compiled, &unknown), Ok(()));
        for (payload, expected) in [
            (
                cat(&[&tag(50, 2), &[9, b'a']]),
                "<root>: declared length exceeds the remaining payload",
            ),
            (
                cat(&[&tag(50, 5), &[1, 2]]),
                "<root>: payload ends before the value is complete",
            ),
            (
                cat(&[&tag(50, 1), &[1; 7]]),
                "<root>: payload ends before the value is complete",
            ),
            (
                cat(&[&tag(50, 0), &[0x80]]),
                "<root>: payload ends before the value is complete",
            ),
        ] {
            assert_eq!(violation(&compiled, &payload), expected, "{payload:?}");
        }
    }

    #[test]
    fn repeated_scalars_accept_packed_and_unpacked_runs() {
        let compiled = compile(&proto3(
            "repeated int32 a = 1; repeated sint32 b = 2; repeated fixed32 c = 3; repeated double d = 4; repeated bool e = 5; repeated string s = 6; int32 single = 7;",
        ));
        let packed = cat(&[
            &ld(1, &cat(&[&varint(1), &varint(300), &varint(-1i64 as u64)])),
            &ld(2, &[0, 1, 2]),
            &ld(3, &[0; 8]),
            &ld(4, &[0; 16]),
            &ld(5, &[0, 1, 1]),
            &ld(6, b"x"),
            &ld(6, b"y"),
        ]);
        assert_eq!(check(&compiled, &packed), Ok(()));
        let unpacked = cat(&[
            &vf(1, 1),
            &vf(1, 2),
            &cat(&[&tag(3, 5), &[0; 4]]),
            &cat(&[&tag(4, 1), &[0; 8]]),
        ]);
        assert_eq!(check(&compiled, &unpacked), Ok(()));
        // The two forms may be mixed, and a packed run may be empty.
        assert_eq!(
            check(&compiled, &cat(&[&ld(1, &[]), &vf(1, 5), &ld(1, &[7])])),
            Ok(())
        );
        for (payload, expected) in [
            (
                ld(3, &[0; 5]),
                "/c: packed field does not hold a whole number of elements",
            ),
            (
                ld(4, &[0; 12]),
                "/d: packed field does not hold a whole number of elements",
            ),
            (
                ld(1, &[0x80]),
                "/a: payload ends before the value is complete",
            ),
            (
                ld(1, &cat(&[&varint(1), &varint(2_147_483_648)])),
                "/a: value does not fit the declared type",
            ),
            (ld(5, &[0, 2]), "/e: boolean is neither 0 nor 1"),
            (
                ld(1, &[0xff; 11]),
                "/a: integer encoding is longer than 10 bytes or overflows 64 bits",
            ),
            // A singular field is not packed.
            (
                ld(7, &[1]),
                "/single: wire type does not match the type declared for the field",
            ),
            // A string run is never packed.
            (
                vf(6, 1),
                "/s: wire type does not match the type declared for the field",
            ),
        ] {
            assert_eq!(violation(&compiled, &payload), expected, "{payload:?}");
        }
    }

    #[test]
    fn a_packed_run_that_runs_past_its_message_is_a_violation_not_a_read_of_the_next_field() {
        let compiled = compile(&proto3("repeated int32 a = 1; Inner inner = 2; message Inner { repeated int32 x = 1; } int32 after = 3;"));
        // The inner message ends in the middle of a multi-byte varint of the packed run.
        let inner = ld(1, &[0x81]);
        let payload = cat(&[&ld(2, &inner), &vf(3, 1)]);
        assert_eq!(
            violation(&compiled, &payload),
            "/inner/x: payload ends before the value is complete"
        );
    }

    #[test]
    fn nested_messages_maps_and_oneofs_are_walked_with_their_paths() {
        let text = fixtures::read("protobuf", "order.proto");
        let compiled = compile(&text);
        let line = |inner: &[u8]| ld(5, inner);
        assert_eq!(
            violation(&compiled, &line(&ld(1, &[0xff]))),
            "/lines/sku: string is not valid UTF-8"
        );
        assert_eq!(
            violation(&compiled, &ld(8, &ld(2, b"x"))),
            "/attrs/value: wire type does not match the type declared for the field"
        );
        assert_eq!(
            violation(&compiled, &ld(8, &ld(1, &[0xff]))),
            "/attrs/key: string is not valid UTF-8"
        );
        assert_eq!(
            violation(&compiled, &ld(9, &[0xff])),
            "/email: string is not valid UTF-8"
        );
        // Both members of the oneof may arrive (the last wins in a reader).
        assert_eq!(check(&compiled, &cat(&[&ld(9, b"a"), &vf(10, 3)])), Ok(()));
        // A nested message may be split over several occurrences.
        assert_eq!(
            check(&compiled, &cat(&[&line(&ld(1, b"a")), &line(&vf(2, 3))])),
            Ok(())
        );
        // An empty nested message and an empty payload are valid.
        assert_eq!(check(&compiled, &line(&[])), Ok(()));
        assert_eq!(check(&compiled, &[]), Ok(()));
    }

    #[test]
    fn open_enums_accept_any_int32_and_closed_enums_only_declared_values() {
        let open = compile("syntax = \"proto3\"; message M { E e = 1; repeated E es = 2; enum E { Z = 0; A = 1; } }");
        for value in [0u64, 1, 2, 4_000_000, -9i64 as u64] {
            assert_eq!(check(&open, &vf(1, value)), Ok(()), "{value}");
        }
        assert_eq!(
            violation(&open, &vf(1, 2_147_483_648)),
            "/e: value does not fit the declared type"
        );
        assert_eq!(check(&open, &ld(2, &[0, 1, 9])), Ok(()));
        let closed = compile("syntax = \"proto2\"; message M { optional E e = 1; repeated E es = 2; enum E { A = 1; B = 2; N = -5; } }");
        for value in [1u64, 2, -5i64 as u64] {
            assert_eq!(check(&closed, &vf(1, value)), Ok(()), "{value}");
        }
        assert_eq!(
            violation(&closed, &vf(1, 3)),
            "/e: enum value is not declared in the closed enum"
        );
        assert_eq!(
            violation(&closed, &vf(1, 0)),
            "/e: enum value is not declared in the closed enum"
        );
        assert_eq!(
            violation(&closed, &ld(2, &[1, 2, 7])),
            "/es: enum value is not declared in the closed enum"
        );
        assert_eq!(check(&closed, &ld(2, &[1, 2])), Ok(()));
    }

    #[test]
    fn required_fields_must_be_present_in_every_message_that_has_them() {
        let compiled = compile(
            "syntax = \"proto2\"; message M { required int32 a = 1; required string b = 2; optional Inner i = 3; repeated Inner r = 4; \
             message Inner { required int32 x = 1; optional int32 y = 2; } }",
        );
        let ok = cat(&[&vf(1, 1), &ld(2, b"b")]);
        assert_eq!(check(&compiled, &ok), Ok(()));
        // Order does not matter; a repeated occurrence of a required field is accepted.
        assert_eq!(
            check(&compiled, &cat(&[&ld(2, b"b"), &vf(1, 1), &vf(1, 2)])),
            Ok(())
        );
        assert_eq!(violation(&compiled, &[]), "/a: required field is missing");
        assert_eq!(
            violation(&compiled, &vf(1, 1)),
            "/b: required field is missing"
        );
        assert_eq!(
            violation(&compiled, &ld(2, b"b")),
            "/a: required field is missing"
        );
        let with = |inner: &[u8]| cat(&[&ok, &ld(3, inner)]);
        assert_eq!(check(&compiled, &with(&vf(1, 4))), Ok(()));
        assert_eq!(
            violation(&compiled, &with(&vf(2, 4))),
            "/i/x: required field is missing"
        );
        assert_eq!(
            violation(&compiled, &with(&[])),
            "/i/x: required field is missing"
        );
        assert_eq!(
            violation(&compiled, &cat(&[&ok, &ld(4, &vf(1, 1)), &ld(4, &[])])),
            "/r/x: required field is missing"
        );
        // A required field is present even when its value is the default.
        assert_eq!(check(&compiled, &cat(&[&vf(1, 0), &ld(2, b"")])), Ok(()));
    }

    #[test]
    fn a_recursive_schema_is_walked_to_a_bounded_depth_without_recursion() {
        let compiled = std::sync::Arc::new(compile(
            "syntax = \"proto3\"; message N { N next = 1; int32 v = 2; }",
        ));
        let nested = |depth: usize| {
            let mut inner = vf(2, 7);
            for _ in 0..depth {
                inner = ld(1, &inner);
            }
            inner
        };
        // The root plus MAX_VALUE_DEPTH - 1 children.
        assert_eq!(check(&compiled, &nested(MAX_VALUE_DEPTH - 1)), Ok(()));
        let too_deep = check(&compiled, &nested(MAX_VALUE_DEPTH));
        assert!(
            matches!(&too_deep, Err(SchemaError::LimitExceeded(m)) if m.ends_with(": value is nested too deeply to check")),
            "{too_deep:?}"
        );
        // The walk uses an explicit stack, so a 64 KiB thread is plenty for
        // a payload that nests a hundred thousand levels.
        let payload = nested(100_000);
        let c = compiled.clone();
        let verdict = on_stack(64, move || check(&c, &payload));
        assert!(
            matches!(verdict, Err(SchemaError::LimitExceeded(_))),
            "{verdict:?}"
        );
    }

    #[test]
    fn an_over_deep_limit_is_never_reported_as_a_violation() {
        let compiled = compile("syntax = \"proto3\"; message N { N next = 1; }");
        let mut inner = Vec::new();
        for _ in 0..200 {
            inner = ld(1, &inner);
        }
        let verdict = check(&compiled, &inner);
        assert!(
            matches!(&verdict, Err(SchemaError::LimitExceeded(_))),
            "{verdict:?}"
        );
        // The same message with a framing error below the cap is a violation.
        let mut bad = vec![0x80];
        for _ in 0..10 {
            bad = ld(1, &bad);
        }
        assert!(matches!(
            check(&compiled, &bad),
            Err(SchemaError::Violation(_))
        ));
    }

    #[test]
    fn a_batch_shares_one_validation_budget() {
        let compiled = compile(&proto3("repeated int32 a = 1;"));
        let payload = ld(1, &vec![1u8; 1000]);
        let mut shared = ValidationBudget {
            org: String::new(),
            remaining: 2500,
        };
        // Each document costs its tag (1) plus its packed bytes (1000); the third does not fit.
        assert_eq!(
            PROTOBUF_OPS.validate_metered(&compiled, &payload, &mut shared),
            Ok(())
        );
        assert_eq!(
            PROTOBUF_OPS.validate_metered(&compiled, &payload, &mut shared),
            Ok(())
        );
        assert_eq!(shared.remaining, 2500 - 2 * 1001);
        let third = PROTOBUF_OPS.validate_metered(&compiled, &payload, &mut shared);
        assert!(
            matches!(&third, Err(SchemaError::LimitExceeded(m)) if m.ends_with(": document exceeds the validation work budget")),
            "{third:?}"
        );
        // A refused charge is not debited.
        assert_eq!(shared.remaining, 2500 - 2 * 1001 - 1);
    }

    // -------------------------------------------------------------------------
    // Compatibility
    // -------------------------------------------------------------------------

    fn compat(old: &str, new: &str, mode: Compatibility) -> Result<(), SchemaError> {
        PROTOBUF_OPS.check_compatibility(old, new, mode)
    }

    fn incompatible(old: &str, new: &str, mode: Compatibility) -> String {
        match compat(old, new, mode) {
            Err(SchemaError::Incompatible(m)) => m,
            other => panic!("expected an incompatibility for {mode:?}: {other:?}"),
        }
    }

    #[test]
    fn compatibility_matrix_for_proto3_fields() {
        use Compatibility::{Backward, Forward, Full};
        const OLD: &str =
            "int32 a = 1; string b = 2; Inner c = 3; repeated int32 d = 4; Color e = 5; \
                           message Inner { int32 x = 1; } enum Color { Z = 0; A = 1; }";
        let old = proto3(OLD);
        // (label, new body, backward ok, forward ok)
        for (label, new_body, backward, forward) in [
            ("identical", OLD.to_string(), true, true),
            (
                "a field is added",
                format!("{OLD} int64 added = 6;"),
                true,
                true,
            ),
            (
                "a field is removed",
                OLD.replace("string b = 2; ", ""),
                true,
                true,
            ),
            (
                "a field is renamed",
                OLD.replace("string b = 2;", "string renamed = 2;"),
                true,
                true,
            ),
            (
                "a field is moved into a oneof",
                OLD.replace("int32 a = 1;", "oneof o { int32 a = 1; }"),
                true,
                true,
            ),
            (
                "a field becomes proto3 optional",
                OLD.replace("int32 a = 1;", "optional int32 a = 1;"),
                true,
                true,
            ),
            (
                "a nested field is added",
                OLD.replace("int32 x = 1;", "int32 x = 1; int32 y = 2;"),
                true,
                true,
            ),
            (
                "an enum gains a value",
                OLD.replace("A = 1;", "A = 1; B = 2;"),
                true,
                true,
            ),
            (
                "an enum loses a value",
                OLD.replace("A = 1;", ""),
                true,
                true,
            ),
            (
                "int32 becomes int64",
                OLD.replace("int32 a = 1;", "int64 a = 1;"),
                false,
                false,
            ),
            (
                "int32 becomes uint32",
                OLD.replace("int32 a = 1;", "uint32 a = 1;"),
                false,
                false,
            ),
            (
                "int32 becomes sint32",
                OLD.replace("int32 a = 1;", "sint32 a = 1;"),
                false,
                false,
            ),
            (
                "int32 becomes bool",
                OLD.replace("int32 a = 1;", "bool a = 1;"),
                false,
                false,
            ),
            (
                "string becomes bytes",
                OLD.replace("string b = 2;", "bytes b = 2;"),
                false,
                false,
            ),
            (
                "string becomes a message",
                OLD.replace("string b = 2;", "Inner b = 2;"),
                false,
                false,
            ),
            (
                "a singular field becomes repeated",
                OLD.replace("int32 a = 1;", "repeated int32 a = 1;"),
                false,
                false,
            ),
            (
                "a repeated field becomes singular",
                OLD.replace("repeated int32 d = 4;", "int32 d = 4;"),
                false,
                false,
            ),
            (
                "a message becomes an enum",
                OLD.replace("Inner c = 3;", "Color c = 3;"),
                false,
                false,
            ),
            (
                "an enum becomes int32",
                OLD.replace("Color e = 5;", "int32 e = 5;"),
                false,
                false,
            ),
            (
                "a nested field changes type",
                OLD.replace("int32 x = 1;", "string x = 1;"),
                false,
                false,
            ),
            (
                "a field changes number",
                OLD.replace("string b = 2;", "string b = 9;"),
                true,
                true,
            ),
        ] {
            let new = proto3(&new_body);
            assert_eq!(
                compat(&old, &new, Backward).is_ok(),
                backward,
                "{label}: backward"
            );
            assert_eq!(
                compat(&old, &new, Forward).is_ok(),
                forward,
                "{label}: forward"
            );
            assert_eq!(
                compat(&old, &new, Full).is_ok(),
                backward && forward,
                "{label}: full"
            );
            assert_eq!(
                compat(&old, &new, Compatibility::None),
                Ok(()),
                "{label}: none"
            );
        }
    }

    #[test]
    fn compatibility_messages_name_the_field_and_the_types() {
        use Compatibility::{Backward, Forward};
        let old = proto3("int32 a = 1; Inner c = 3; message Inner { int32 x = 1; }");
        let new = proto3("int64 a = 1; Inner c = 3; message Inner { string x = 1; }");
        assert_eq!(
            incompatible(&old, &new, Backward),
            "backward: data written under the old schema may not be readable with the new one - /a: int32 written by the old schema cannot be read as int64 by the new schema"
        );
        assert_eq!(
            incompatible(&old, &new, Forward),
            "forward: data written under the new schema may not be readable with the old one - /a: int64 written by the new schema cannot be read as int32 by the old schema"
        );
        let nested = proto3("int32 a = 1; Inner c = 3; message Inner { string x = 1; }");
        assert_eq!(
            incompatible(&old, &nested, Backward),
            "backward: data written under the old schema may not be readable with the new one - /c/x: int32 written by the old schema cannot be read as string by the new schema"
        );
        let repeated = proto3("repeated int32 a = 1; Inner c = 3; message Inner { int32 x = 1; }");
        assert!(incompatible(&old, &repeated, Backward)
            .ends_with("/a: int32 written by the old schema cannot be read as repeated int32 by the new schema"));
        let renamed = proto3(
            "int32 a = 1; Other c = 3; message Other { int32 x = 1; } enum Unused { U = 0; }",
        );
        assert_eq!(
            compat(&old, &renamed, Compatibility::Full),
            Ok(()),
            "types are matched by field number, not by name"
        );
    }

    #[test]
    fn maps_keep_their_key_and_value_types() {
        use Compatibility::Backward;
        let old = proto3(
            "map<string, int32> m = 1; map<int32, Inner> n = 2; message Inner { int32 x = 1; }",
        );
        assert_eq!(compat(&old, &old, Backward), Ok(()));
        for new_body in [
            "map<string, int64> m = 1; map<int32, Inner> n = 2; message Inner { int32 x = 1; }",
            "map<int32, int32> m = 1; map<int32, Inner> n = 2; message Inner { int32 x = 1; }",
            "repeated Entry m = 1; map<int32, Inner> n = 2; message Entry { string key = 1; int32 value = 2; } message Inner { int32 x = 1; }",
            "map<string, int32> m = 1; map<int32, Inner> n = 2; message Inner { string x = 1; }",
            "map<string, int32> m = 1; int32 n = 2; message Inner { int32 x = 1; }",
        ] {
            assert!(
                matches!(compat(&old, &proto3(new_body), Backward), Err(SchemaError::Incompatible(_))),
                "{new_body}"
            );
        }
    }

    #[test]
    fn a_closed_enum_needs_every_value_the_other_side_can_write() {
        use Compatibility::{Backward, Forward};
        let two = proto2("optional E e = 1; enum E { A = 1; B = 2; }");
        let three = proto2("optional E e = 1; enum E { A = 1; B = 2; C = 3; }");
        // New reads old: the old values must all be known.
        assert_eq!(compat(&two, &three, Backward), Ok(()));
        assert_eq!(
            incompatible(&three, &two, Backward),
            "backward: data written under the old schema may not be readable with the new one - /e: enum 'E': value 3 of the old schema is unknown to the new schema, whose enum is closed"
        );
        // Old reads new.
        assert_eq!(compat(&three, &two, Forward), Ok(()));
        assert_eq!(
            incompatible(&two, &three, Forward),
            "forward: data written under the new schema may not be readable with the old one - /e: enum 'E': value 3 of the new schema is unknown to the old schema, whose enum is closed"
        );
        // An open enum writer cannot be bounded by a closed reader.
        let open = "syntax = \"proto3\"; message M { E e = 1; enum E { Z = 0; A = 1; } }";
        assert!(incompatible(open, &two, Backward)
            .contains("accepts any number, but the enum of the new schema is closed"));
        assert_eq!(compat(&two, open, Backward), Ok(()));
    }

    #[test]
    fn a_required_field_must_be_guaranteed_by_the_writer() {
        use Compatibility::{Backward, Forward};
        let optional = proto2("optional int32 a = 1;");
        let required = proto2("required int32 a = 1;");
        let extra_required = proto2("optional int32 a = 1; required string b = 2;");
        // Optional -> required: new readers cannot rely on old data.
        let why = incompatible(&optional, &required, Backward);
        assert!(why.ends_with("/a: the new schema requires field 'a' (number 1) of message 'M', which the old schema does not always write"), "{why}");
        assert_eq!(compat(&optional, &required, Forward), Ok(()));
        // Required -> optional: old readers cannot rely on new data.
        assert_eq!(compat(&required, &optional, Backward), Ok(()));
        let why = incompatible(&required, &optional, Forward);
        assert!(why.ends_with("/a: the old schema requires field 'a' (number 1) of message 'M', which the new schema does not always write"), "{why}");
        // A new required field is a backward break; dropping a required one is a forward break.
        assert!(incompatible(&optional, &extra_required, Backward)
            .contains("requires field 'b' (number 2)"));
        assert_eq!(compat(&optional, &extra_required, Forward), Ok(()));
        let why = incompatible(&extra_required, &optional, Forward);
        assert!(
            why.contains("the old schema requires field 'b' (number 2)"),
            "{why}"
        );
        assert_eq!(compat(&extra_required, &optional, Backward), Ok(()));
        // A nested required field is found through the field that holds it.
        let nested_old = proto2("optional Inner i = 1; message Inner { optional int32 x = 1; }");
        let nested_new = proto2("optional Inner i = 1; message Inner { required int32 x = 1; }");
        assert!(incompatible(&nested_old, &nested_new, Backward)
            .contains("/i/x: the new schema requires field 'x'"));
    }

    #[test]
    fn a_number_the_old_schema_reserved_cannot_come_back() {
        use Compatibility::{Backward, Forward, Full};
        let old = proto3("int32 a = 1; reserved 5, 7 to 9; reserved \"gone\";");
        for mode in [Backward, Forward, Full] {
            let why = incompatible(&old, &proto3("int32 a = 1; string again = 5;"), mode);
            assert!(why.ends_with("/again: field number 5 is reserved by the old schema and cannot be used by the new one"), "{mode:?}: {why}");
            assert!(
                incompatible(&old, &proto3("int32 a = 1; string again = 8;"), mode)
                    .contains("field number 8 is reserved")
            );
        }
        // Other numbers, and removing a field without reserving it, are fine.
        assert_eq!(
            compat(
                &old,
                &proto3("int32 a = 1; string fresh = 6; reserved 5;"),
                Full
            ),
            Ok(())
        );
        assert_eq!(
            compat(
                "syntax = \"proto3\"; message M { int32 a = 1; int32 b = 2; }",
                &proto3("int32 a = 1;"),
                Compatibility::Full
            ),
            Ok(())
        );
        // Reservation inside a nested message counts too.
        let nested_old = proto3("Inner i = 1; message Inner { int32 x = 1; reserved 2; }");
        let nested_new = proto3("Inner i = 1; message Inner { int32 x = 1; string y = 2; }");
        assert!(incompatible(&nested_old, &nested_new, Backward)
            .contains("/i/y: field number 2 is reserved"));
    }

    #[test]
    fn compatibility_follows_the_designated_message_and_survives_recursion() {
        use Compatibility::Full;
        let old = "// tentabus:message Order\nsyntax = \"proto3\"; message Helper { int32 h = 1; } message Order { Order next = 1; int32 v = 2; }";
        let same = "syntax = \"proto3\"; message Order { Order next = 1; int32 v = 2; }";
        assert_eq!(compat(old, same, Full), Ok(()));
        let changed = "syntax = \"proto3\"; message Order { Order next = 1; string v = 2; }";
        assert!(incompatible(old, changed, Full).contains("/v: int32 written by"));
        // A recursive message pair is compared once, however deep the cycle.
        let deep =
            "syntax = \"proto3\"; message Order { Order next = 1; Order prev = 3; int32 v = 2; }";
        assert_eq!(compat(deep, deep, Full), Ok(()));
    }

    #[test]
    fn a_comparison_that_runs_out_of_budget_is_limit_exceeded_not_compatible() {
        let text = proto3("Inner a = 1; Inner b = 2; Inner c = 3; message Inner { int32 x = 1; int32 y = 2; int32 z = 3; }");
        let compiled = parse_text(&text).unwrap();
        assert!(reads(&compiled, &compiled, true, MAX_COMPAT_WORK).is_ok());
        assert!(matches!(
            reads(&compiled, &compiled, true, 5),
            Err(Stop::Complex)
        ));
        // Unparseable texts are refusals, whatever the mode but none.
        assert!(matches!(
            compat("nonsense", &text, Compatibility::Backward),
            Err(SchemaError::Invalid(_))
        ));
        assert_eq!(compat("nonsense", "nonsense", Compatibility::None), Ok(()));
    }

    // -------------------------------------------------------------------------
    // Projection
    // -------------------------------------------------------------------------

    fn allowed(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_projection_keeps_the_allowed_fields_and_the_types_they_reach() {
        let order = fixtures::read("protobuf", "order.proto");
        let derived = PROTOBUF_OPS
            .derive_subschema(
                &order,
                &allowed(&["id", "status", "lines", "attrs", "email", "phone_code"]),
            )
            .unwrap();
        assert_eq!(
            derived,
            "syntax = \"proto3\";\npackage shop;\n\nmessage Order {\n  message Line {\n    string sku = 1;\n    int32 qty = 2;\n    double price = 3;\n  }\n  enum Status {\n    UNKNOWN = 0;\n    NEW = 1;\n    SHIPPED = 2;\n  }\n  int64 id = 1;\n  .shop.Order.Status status = 4;\n  repeated .shop.Order.Line lines = 5;\n  map<string, int32> attrs = 8;\n  oneof contact {\n    string email = 9;\n    int32 phone_code = 10;\n  }\n}\n"
        );
        // Stable: the same inputs give the same text, and the text is a schema again.
        assert_eq!(
            derived,
            PROTOBUF_OPS
                .derive_subschema(
                    &order,
                    &allowed(&["phone_code", "email", "attrs", "lines", "status", "id"])
                )
                .unwrap()
        );
        let projected = compile(&derived);
        // A full order still validates (the fields the projection dropped are unknown to it).
        let full = fixtures::cases("protobuf", "datum")
            .into_iter()
            .find(|c| c.name == "valid-full")
            .unwrap();
        assert_eq!(check(&projected, &full.payload), Ok(()));
        // The projection checks what it kept.
        assert_eq!(
            violation(&projected, &ld(5, &ld(1, &[0xff]))),
            "/lines/sku: string is not valid UTF-8"
        );
    }

    #[test]
    fn a_projection_without_a_type_does_not_declare_it() {
        let order = fixtures::read("protobuf", "order.proto");
        let derived = PROTOBUF_OPS
            .derive_subschema(&order, &allowed(&["id", "customer"]))
            .unwrap();
        assert_eq!(
            derived,
            "syntax = \"proto3\";\npackage shop;\n\nmessage Order {\n  int64 id = 1;\n  string customer = 2;\n}\n"
        );
        // Unknown names are ignored, and nothing allowed is an empty message that still compiles.
        let empty = PROTOBUF_OPS
            .derive_subschema(&order, &allowed(&["nope"]))
            .unwrap();
        assert_eq!(
            empty,
            "syntax = \"proto3\";\npackage shop;\n\nmessage Order {\n}\n"
        );
        compile(&empty);
    }

    #[test]
    fn a_projection_writes_the_designated_message_first_and_keeps_containers_of_reached_types() {
        let text = "// tentabus:message Order\nsyntax = \"proto3\";\npackage p;\n\
                    message Library { message Book { string title = 1; Color color = 2; } enum Color { Z = 0; R = 1; } message Unused { int32 u = 1; } }\n\
                    message Order { Library.Book book = 1; Library.Color shade = 2; int32 skipped = 3; }\n\
                    enum TopLevel { T = 0; }";
        let derived = PROTOBUF_OPS
            .derive_subschema(text, &allowed(&["book", "shade"]))
            .unwrap();
        assert_eq!(
            derived,
            "syntax = \"proto3\";\npackage p;\n\nmessage Order {\n  .p.Library.Book book = 1;\n  .p.Library.Color shade = 2;\n}\n\nmessage Library {\n  message Book {\n    string title = 1;\n    .p.Library.Color color = 2;\n  }\n  enum Color {\n    Z = 0;\n    R = 1;\n  }\n}\n"
        );
        // The first message is the designated one, so no comment is needed.
        assert_eq!(
            violation(&compile(&derived), &ld(1, &ld(1, &[0xff]))),
            "/book/title: string is not valid UTF-8"
        );
    }

    #[test]
    fn a_proto2_projection_keeps_required_and_optional_labels() {
        let text = proto2("required int32 a = 1; optional string b = 2 [default = \"d\"]; repeated int32 c = 3 [packed = true]; optional E e = 4; enum E { X = 1; Y = 2; }");
        let derived = PROTOBUF_OPS
            .derive_subschema(&text, &allowed(&["a", "c", "e"]))
            .unwrap();
        assert_eq!(
            derived,
            "syntax = \"proto2\";\n\nmessage M {\n  enum E {\n    X = 1;\n    Y = 2;\n  }\n  required int32 a = 1;\n  repeated int32 c = 3;\n  optional .M.E e = 4;\n}\n"
        );
        let projected = compile(&derived);
        assert_eq!(violation(&projected, &[]), "/a: required field is missing");
        assert_eq!(
            violation(&projected, &cat(&[&vf(1, 1), &vf(4, 3)])),
            "/e: enum value is not declared in the closed enum"
        );
        // Dropping the required field drops the requirement.
        let without = PROTOBUF_OPS
            .derive_subschema(&text, &allowed(&["b"]))
            .unwrap();
        assert_eq!(check(&compile(&without), &[]), Ok(()));
    }

    #[test]
    fn a_projection_keeps_enum_aliases_and_explicit_optional() {
        let text = "syntax = \"proto3\"; message M { optional int32 a = 1; E e = 2; enum E { option allow_alias = true; Z = 0; Zero = 0; } }";
        let derived = PROTOBUF_OPS
            .derive_subschema(text, &allowed(&["a", "e"]))
            .unwrap();
        assert!(derived.contains("optional int32 a = 1;"), "{derived}");
        assert!(derived.contains("option allow_alias = true;"), "{derived}");
        compile(&derived);
    }

    // -------------------------------------------------------------------------
    // Adversarial inputs. The limits are loose enough for a debug build under
    // load; the timings are printed (`--nocapture`) for release-profile runs.
    // -------------------------------------------------------------------------

    fn message_chain(depth: usize) -> String {
        let mut text = String::from("syntax = \"proto3\";\n");
        for i in 0..depth {
            text.push_str(&format!("message M{i} {{ "));
        }
        text.push_str(&"} ".repeat(depth));
        text
    }

    #[test]
    fn brackets_nested_too_deep_are_refused_before_the_parser_can_overflow() {
        // 10 000 levels abort a 2 MiB stack inside protobuf-parse (measured);
        // the pre-scan refuses them on a stack a quarter of that.
        let aggregate = format!(
            "syntax = \"proto3\"; option deprecated = {}1{};",
            "{a ".repeat(10_000),
            "}".repeat(10_000)
        );
        let list = format!(
            "syntax = \"proto3\"; option deprecated = {}{};",
            "[".repeat(50_000),
            "]".repeat(50_000)
        );
        for (label, text) in [
            ("messages", message_chain(10_000)),
            ("option aggregate", aggregate),
            ("list constant", list),
        ] {
            let got = timed(label, Duration::from_secs(2), || {
                on_stack(512, move || PROTOBUF_OPS.compile(&text))
            });
            assert!(
                matches!(&got, Err(SchemaError::Invalid(m)) if m == "the schema is nested deeper than 32 levels"),
                "{label}: {got:?}"
            );
        }
    }

    #[test]
    fn the_deepest_accepted_nesting_fits_a_small_stack() {
        // Sixteen nested messages are the most a schema may declare.
        let sixteen = message_chain(16);
        assert!(on_stack(512, move || PROTOBUF_OPS.compile(&sixteen)).is_ok());
        let seventeen = message_chain(17);
        let got = on_stack(512, move || PROTOBUF_OPS.compile(&seventeen));
        assert!(
            matches!(&got, Err(SchemaError::Invalid(m)) if m == "messages are nested deeper than 16 levels"),
            "{got:?}"
        );
        // Bracket depth 32 is the most the parser is asked to recurse through,
        // and it neither overflows nor panics (the value is just not a bool).
        let thirty_one = format!(
            "syntax = \"proto3\"; option deprecated = {}1{};",
            "{a ".repeat(31),
            "}".repeat(31)
        );
        let got = on_stack(512, move || PROTOBUF_OPS.compile(&thirty_one));
        assert!(matches!(got, Err(SchemaError::Invalid(_))));
    }

    #[test]
    fn declaration_caps_hold_exactly_at_their_limit() {
        let many_messages = |n: usize| {
            let mut text = String::from("syntax = \"proto3\";\n");
            for i in 0..n {
                text.push_str(&format!("message M{i} {{}}\n"));
            }
            text
        };
        compile(&many_messages(MAX_MESSAGES));
        assert!(refusal(&many_messages(MAX_MESSAGES + 1))
            .contains("the schema has more than 256 messages"));
        // A map field is a message too.
        let maps = |n: usize| {
            let fields: String = (0..n)
                .map(|i| format!("map<string, int32> m{i} = {}; ", i + 1))
                .collect();
            format!("syntax = \"proto3\"; message M {{ {fields} }}")
        };
        compile(&maps(MAX_MESSAGES - 1));
        assert!(refusal(&maps(MAX_MESSAGES)).contains("the schema has more than 256 messages"));

        let fields = |n: usize| {
            (0..n)
                .map(|i| format!("int32 f{i} = {}; ", i + 1))
                .collect::<String>()
        };
        compile(&proto3(&fields(MAX_FIELDS_PER_MESSAGE)));
        assert!(refusal(&proto3(&fields(MAX_FIELDS_PER_MESSAGE + 1)))
            .contains("message 'M' has more than 1024 fields"));
        let spread = |messages: usize| {
            (0..messages)
                .map(|m| format!("message S{m} {{ {} }}\n", fields(MAX_FIELDS_PER_MESSAGE)))
                .collect::<String>()
        };
        compile(&format!("syntax = \"proto3\";\n{}", spread(4)));
        assert!(refusal(&format!("syntax = \"proto3\";\n{}", spread(5)))
            .contains("the schema has more than 4096 fields"));

        let values = |n: usize| (0..n).map(|i| format!("V{i} = {i}; ")).collect::<String>();
        compile(&format!(
            "syntax = \"proto3\"; enum E {{ {} }} message M {{ E e = 1; }}",
            values(MAX_ENUM_VALUES_PER_ENUM)
        ));
        assert!(refusal(&format!(
            "syntax = \"proto3\"; enum E {{ {} }}",
            values(MAX_ENUM_VALUES_PER_ENUM + 1)
        ))
        .contains("enum 'E' has more than 1024 values"));
        let enums = (0..5)
            .map(|e| format!("enum E{e} {{ {} }}\n", values(1000)))
            .collect::<String>();
        assert!(refusal(&format!("syntax = \"proto3\";\n{enums}"))
            .contains("the schema has more than 4096 enum values"));
        let enums = (0..=MAX_ENUMS)
            .map(|e| format!("enum E{e} {{ Z{e} = 0; }}\n"))
            .collect::<String>();
        assert!(refusal(&format!("syntax = \"proto3\";\n{enums}"))
            .contains("the schema has more than 128 enums"));

        let reserved = |n: usize| {
            (0..n)
                .map(|i| format!("{}", i * 2 + 1))
                .collect::<Vec<_>>()
                .join(", ")
        };
        compile(&proto3(&format!(
            "reserved {};",
            reserved(MAX_RESERVED_RANGES)
        )));
        assert!(refusal(&proto3(&format!(
            "reserved {};",
            reserved(MAX_RESERVED_RANGES + 1)
        )))
        .contains("reserves more than 1024 ranges"));
        let required = |n: usize| {
            (0..n)
                .map(|i| format!("required int32 r{i} = {}; ", i + 1))
                .collect::<String>()
        };
        compile(&proto2(&required(MAX_REQUIRED_FIELDS)));
        assert!(refusal(&proto2(&required(MAX_REQUIRED_FIELDS + 1)))
            .contains("message 'M' has more than 64 required fields"));
        let options = "option x=1;\n".repeat(MAX_ASSIGNMENTS + 1);
        assert!(
            refusal(&format!("syntax = \"proto3\";\n{options}message M {{}}"))
                .contains("more than 12288 assignments")
        );
    }

    #[test]
    fn the_largest_accepted_schema_compiles_in_bounded_time() {
        // 256 messages x 16 fields (4096 fields, every one naming the LAST
        // sibling: the parser's worst scan) and 128 enums x 32 values.
        let mut text = String::from("syntax = \"proto3\";\n");
        for m in 0..MAX_MESSAGES - 1 {
            let fields: String = (0..16)
                .map(|f| format!("Leaf f{f} = {}; ", f + 1))
                .collect();
            text.push_str(&format!("message M{m} {{ {fields} }}\n"));
        }
        text.push_str("message Leaf { int32 leaf = 1; }\n");
        for e in 0..MAX_ENUMS {
            let values: String = (0..32).map(|v| format!("E{e}V{v} = {v}; ")).collect();
            text.push_str(&format!("enum E{e} {{ {values} }}\n"));
        }
        let compiled = timed("largest schema", Duration::from_secs(10), || {
            PROTOBUF_OPS.compile(&text)
        });
        assert!(compiled.is_ok(), "{compiled:?}");
        // The sibling scan is the quadratic part; with 254 siblings and 4064 fields it stays small.
    }

    #[test]
    fn pathological_schema_texts_are_refused_or_accepted_but_never_panic() {
        let long_chain = format!(
            "syntax = \"proto3\"; message M {{ {} }}",
            "int32 a = 1;".repeat(2)
        );
        for text in [
            "\u{feff}syntax = \"proto3\"; message M {}".to_string(),
            "syntax = \"proto3\"; message M { int32 a = 99999999999999999999; }".to_string(),
            "syntax = \"proto3\"; message M { reserved 1 to 99999999999; }".to_string(),
            "syntax = \"proto3\"; message M { reserved 5 to 1; }".to_string(),
            "syntax = \"proto3\"; message M { reserved 1 to max; }".to_string(),
            "syntax = \"proto3\"; message M { map<string, map<string, int32>> m = 1; }".to_string(),
            "syntax = \"proto3\"; message M { oneof o { } }".to_string(),
            "syntax = \"proto3\"; message M { oneof o { oneof p { int32 a = 1; } } }".to_string(),
            "syntax = \"proto3\"; enum E { }".to_string(),
            "syntax = \"proto3\"; message M { message M {} M m = 1; }".to_string(),
            "syntax = \"proto3\"; message M { map<string, int32> a = 1; message AEntry { int32 x = 1; } }".to_string(),
            "syntax = \"proto3\"; message M { map<string, int32> a = 1; map<string, int32> a = 2; }".to_string(),
            "syntax = \"proto3\"; message M { int32 a = 1 [default = 5]; }".to_string(),
            "syntax = \"proto3\"; message M { int32 a = 1; } message N { M.Missing m = 1; }".to_string(),
            "syntax = \"proto3\"; message M { .M m = 1; M.M n = 2; }".to_string(),
            "syntax = \"proto3\"; message M { stream int32 a = 1; }".to_string(),
            "syntax = \"proto3\"; message M { string a = 1 [json_name = \"\\xff\"]; }".to_string(),
            "syntax = \"proto3\"; message M { string a = 1 [json_name = 5]; }".to_string(),
            "syntax = 'proto3'; message M { int32 a = 1; }".to_string(),
            "syntax = \"proto3\" message M {}".to_string(),
            "message".to_string(),
            "{".to_string(),
            "}".to_string(),
            ";;;;;;;".to_string(),
            "syntax = \"proto3\"; message M { int32 a = 1; ;;;; }".to_string(),
            "syntax = \"proto3\"; message M { int32 a = 1 [deprecated = true, deprecated = false]; }".to_string(),
            "syntax = \"proto3\"; package a..b; message M {}".to_string(),
            "syntax = \"proto3\"; package ; message M {}".to_string(),
            "syntax = \"proto3\"; message M { optional optional int32 a = 1; }".to_string(),
            "syntax = \"proto3\"; message M { repeated optional int32 a = 1; }".to_string(),
            "syntax = \"proto3\"; message M { int32 a = 1; } option java_package = ;".to_string(),
            "syntax = \"proto3\"; message M { int32 \u{0}a = 1; }".to_string(),
            "syntax = \"proto3\"; message M { string a = 1 [json_name = \"\\".to_string(),
            long_chain,
        ] {
            let _ = timed("pathological text", Duration::from_secs(2), || {
                on_stack(512, move || PROTOBUF_OPS.compile(&text).map(|_| ()))
            });
        }
    }

    /// Seeds for the schema fuzz: the fixtures and two files that use every
    /// construct the parser has a name table for.
    fn fuzz_seeds() -> Vec<String> {
        vec![
            fixtures::read("protobuf", "order.proto"),
            fixtures::read("protobuf/proto2", "legacy.proto"),
            "syntax = \"proto3\"; package p.q;\n\
             message A { message B { enum E { Z = 0; Y = 1; } E e = 1; map<string, B> m = 2; repeated C c = 3;\n\
               oneof o { int32 x = 4; string y = 5; } reserved 9, 11 to 13; reserved \"q\"; }\n\
               B b = 1; optional C c = 2; }\n\
             message C { A.B b = 1; A a = 2; map<int32, C> cs = 3; map<string, int32> foo_bar = 4; }\n\
             enum Top { T0 = 0; T1 = 1; }"
                .to_string(),
            "syntax = \"proto2\"; message M { required int32 a = 1; optional M child = 2; repeated N n = 3;\n\
               optional string s = 5 [default = \"x\"]; message N { optional int32 z = 1; } enum K { A = 1; } }"
                .to_string(),
        ]
    }

    /// One structural or lexical mutation of a schema text, chosen by `rng`.
    fn mutate_schema(text: &mut String, rng: &mut Xorshift) {
        const VOCABULARY: [&str; 40] = [
            "message ",
            "enum ",
            "oneof ",
            "map<",
            "string,",
            "int32>",
            "{",
            "}",
            "[",
            "]",
            "(",
            ")",
            ";",
            "=",
            ",",
            "1",
            "99999999999",
            "optional ",
            "repeated ",
            "required ",
            "import ",
            "reserved ",
            "extend ",
            "group ",
            "option ",
            "\"",
            "'",
            "/*",
            "*/",
            "//",
            "\n",
            ".",
            "-",
            "A.B",
            "Entry",
            "AEntry",
            "MEntry",
            "_",
            "max",
            "to ",
        ];
        let boundary_before = |text: &String, at: usize| {
            (0..=at.min(text.len()))
                .rev()
                .find(|i| text.is_char_boundary(*i))
                .unwrap()
        };
        let lines = |text: &String| text.lines().map(str::to_string).collect::<Vec<_>>();
        match rng.below(8) {
            0 => {
                let mut v = lines(text);
                if !v.is_empty() {
                    let i = rng.below(v.len());
                    v.insert(i, v[i].clone());
                    *text = v.join("\n");
                }
            }
            1 => {
                let mut v = lines(text);
                if v.len() > 2 {
                    let (i, j) = (rng.below(v.len()), rng.below(v.len()));
                    v.swap(i, j);
                    *text = v.join("\n");
                }
            }
            2 => {
                let mut v = lines(text);
                if v.len() > 2 {
                    v.remove(rng.below(v.len()));
                    *text = v.join("\n");
                }
            }
            3 => {
                // Make two names the same: the collisions the parser trips on.
                let bytes = text.as_bytes();
                let (mut idents, mut i) = (Vec::new(), 0);
                while i < bytes.len() {
                    if bytes[i].is_ascii_alphabetic() {
                        let start = i;
                        while i < bytes.len()
                            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                        {
                            i += 1;
                        }
                        idents.push((start, i));
                    } else {
                        i += 1;
                    }
                }
                if idents.len() > 2 {
                    let (from, to) = (
                        idents[rng.below(idents.len())],
                        idents[rng.below(idents.len())],
                    );
                    let word = text[to.0..to.1].to_string();
                    text.replace_range(from.0..from.1, &word);
                }
            }
            4 => {
                let at = boundary_before(text, rng.below(text.len() + 1));
                text.insert_str(at, VOCABULARY[rng.below(VOCABULARY.len())]);
            }
            5 => {
                let at = boundary_before(text, rng.below(text.len().max(1)));
                let end = (at + 1 + rng.below(6)).min(text.len());
                if text.is_char_boundary(end) {
                    text.replace_range(at..end, "");
                }
            }
            6 => {
                let digits: Vec<usize> = text
                    .bytes()
                    .enumerate()
                    .filter(|(_, b)| b.is_ascii_digit())
                    .map(|(i, _)| i)
                    .collect();
                if !digits.is_empty() {
                    let at = digits[rng.below(digits.len())];
                    let number = [
                        "0",
                        "1",
                        "2",
                        "3",
                        "19000",
                        "536870911",
                        "536870912",
                        "99999999999",
                    ][rng.below(8)];
                    text.replace_range(at..at + 1, number);
                }
            }
            _ => {
                let at = boundary_before(text, rng.below(text.len() + 1));
                let mut end = (at + 1 + rng.below(60)).min(text.len());
                while !text.is_char_boundary(end) {
                    end += 1;
                }
                let chunk = text[at..end].to_string();
                let to = boundary_before(text, rng.below(text.len() + 1));
                text.insert_str(to, &chunk);
            }
        }
    }

    #[test]
    fn mutated_schema_texts_never_panic_and_stay_fast() {
        // A release build aborts on a panic (`panic = "abort"`), so the
        // `catch_unwind` around the parser cannot save it there: no mutation
        // may reach a panic at all, which is what `PARSER_PANIC` would say.
        let seeds = fuzz_seeds();
        let mut rng = Xorshift(0x9e37_79b9_7f4a_7c15);
        let started = Instant::now();
        let mut accepted = 0;
        let iterations = 4000;
        for _ in 0..iterations {
            let mut text = seeds[rng.below(seeds.len())].clone();
            for _ in 0..1 + rng.below(4) {
                mutate_schema(&mut text, &mut rng);
            }
            let one = Instant::now();
            let shown = text.clone();
            let verdict = on_stack(512, move || PROTOBUF_OPS.compile(&text).map(|_| ()));
            assert!(
                one.elapsed() < Duration::from_secs(2),
                "one mutation took {:?}: {shown}",
                one.elapsed()
            );
            match verdict {
                Ok(()) => accepted += 1,
                Err(SchemaError::Invalid(m)) => {
                    assert_ne!(m, PARSER_PANIC, "the parser panicked on: {shown}")
                }
                Err(other) => panic!("{other:?}"),
            }
        }
        eprintln!(
            "timing: {iterations} mutated schemas: {:?}, {accepted} accepted",
            started.elapsed()
        );
        assert!(
            accepted > iterations / 50 && accepted < iterations,
            "the mutations should hit both outcomes: {accepted}"
        );
    }

    #[test]
    fn a_map_entry_and_a_group_cannot_collide_with_a_type_of_the_scope() {
        // Each of these makes protobuf-parse panic (option_resolver.rs, an
        // `unwrap`), which a release build cannot catch.
        let mut misses = Vec::new();
        for (text, phrase) in [
            (
                "syntax = \"proto3\"; message M { map<string, int32> a = 1; message AEntry { int32 x = 1; } }",
                "type 'AEntry' is defined more than once",
            ),
            (
                "syntax = \"proto3\"; message M { message AEntry { int32 x = 1; } map<string, int32> a = 1; }",
                "type 'AEntry' is defined more than once",
            ),
            (
                "syntax = \"proto3\"; message M { map<string, int32> foo_bar = 1; map<int32, string> fooBar = 2; }",
                "type 'FooBarEntry' is defined more than once",
            ),
            (
                "syntax = \"proto3\"; message M { map<string, int32> a = 1; enum AEntry { Z = 0; } }",
                "type 'AEntry' is defined more than once",
            ),
            (
                "syntax = \"proto2\"; message M { optional group G = 1 { optional int32 z = 1; } message G { optional int32 y = 1; } }",
                "groups are not supported",
            ),
            (
                "syntax = \"proto2\"; message M { oneof o { group G = 1 { optional int32 z = 1; } } }",
                "groups are not supported",
            ),
            (
                "syntax = \"proto2\"; message M { oneof o { group G = 1 { optional int32 z = 1; } } message G { optional int32 y = 1; } }",
                "groups are not supported",
            ),
        ] {
            let got = match PROTOBUF_OPS.compile(text) {
                Err(SchemaError::Invalid(m)) => m,
                other => format!("accepted: {other:?}").chars().take(40).collect(),
            };
            if !got.contains(phrase) {
                misses.push(format!("{text}: {got}"));
            }
        }
        assert_eq!(misses, Vec::<String>::new());
        // The same names in different scopes, and a field called `group`, are fine.
        compile("syntax = \"proto3\"; message M { map<string, int32> a = 1; } message N { message AEntry { int32 x = 1; } }");
        compile("syntax = \"proto3\"; message M { string group = 1; map<string, int32> a = 2; map<string, int32> b = 3; }");
    }

    #[test]
    fn length_bombs_are_refused_at_once() {
        let compiled = compile(&fixtures::read("protobuf", "order.proto"));
        let bomb = |len: u64| cat(&[&tag(2, 2), &varint(len)]);
        for len in [u64::MAX, 1 << 63, 1 << 40, u64::from(u32::MAX), 1 << 20] {
            assert_eq!(
                timed("length bomb", Duration::from_millis(100), || violation(
                    &compiled,
                    &bomb(len)
                )),
                "/customer: declared length exceeds the remaining payload"
            );
        }
        // Inside a nested message the length is checked against the nested message.
        let nested = ld(5, &cat(&[&tag(1, 2), &varint(10), b"abc"]));
        assert_eq!(
            violation(&compiled, &nested),
            "/lines/sku: declared length exceeds the remaining payload"
        );
        // A nested message cannot claim more than its parent has left.
        let outer = cat(&[&tag(5, 2), &varint(1000), b"abc"]);
        assert_eq!(
            violation(&compiled, &outer),
            "/lines: declared length exceeds the remaining payload"
        );
    }

    #[test]
    fn a_payload_of_millions_of_tiny_fields_is_walked_in_bounded_time() {
        let compiled = compile(&proto3(
            "int64 id = 1; repeated int32 xs = 2; string s = 3;",
        ));
        let payload: Vec<u8> = [0x08u8, 0x01]
            .iter()
            .copied()
            .cycle()
            .take(4 << 20)
            .collect();
        assert_eq!(
            timed("2M small fields", Duration::from_secs(5), || check(
                &compiled, &payload
            )),
            Ok(())
        );
        // Unknown fields cost the same.
        let unknown: Vec<u8> = [0xa2u8, 0x06, 0x00]
            .iter()
            .copied()
            .cycle()
            .take(3 << 20)
            .collect();
        assert_eq!(
            timed("1M unknown fields", Duration::from_secs(5), || check(
                &compiled, &unknown
            )),
            Ok(())
        );
        // Empty nested messages: one tag and one length byte each.
        let nested = compile(&proto3("repeated M m = 1;"));
        let empties: Vec<u8> = [0x0au8, 0x00]
            .iter()
            .copied()
            .cycle()
            .take(4 << 20)
            .collect();
        assert_eq!(
            timed("2M empty messages", Duration::from_secs(5), || check(
                &nested, &empties
            )),
            Ok(())
        );
    }

    #[test]
    fn a_packed_bomb_is_charged_before_it_is_iterated() {
        let compiled = compile(&proto3("repeated int32 xs = 1;"));
        let run = |elements: usize| ld(1, &vec![1u8; elements]);
        let fits = run(8 << 20);
        assert_eq!(
            timed("8M packed", Duration::from_secs(5), || check(
                &compiled, &fits
            )),
            Ok(())
        );
        // 32 MiB of one-byte varints is more than a document may spend (20M units).
        let bomb = run(32 << 20);
        let got = timed("32M packed refused", Duration::from_millis(500), || {
            check(&compiled, &bomb)
        });
        assert!(
            matches!(&got, Err(SchemaError::LimitExceeded(m)) if m == "/xs: document exceeds the validation work budget"),
            "{got:?}"
        );
        // Fixed-width runs are charged by bytes too and cost nothing to skip.
        let doubles = compile(&proto3("repeated double xs = 1;"));
        assert_eq!(
            timed("8M of doubles", Duration::from_millis(500), || check(
                &doubles,
                &ld(1, &vec![0u8; 8 << 20])
            )),
            Ok(())
        );
    }

    #[test]
    fn a_long_string_is_checked_for_utf8_without_copying_it() {
        let compiled = compile(&proto3("string s = 1; bytes b = 2;"));
        let text = "ż".repeat(4 << 20);
        assert_eq!(
            timed("8 MiB string", Duration::from_secs(2), || check(
                &compiled,
                &ld(1, text.as_bytes())
            )),
            Ok(())
        );
        let mut broken = text.into_bytes();
        let last = broken.len() - 1;
        broken[last] = 0xff;
        assert_eq!(
            violation(&compiled, &ld(1, &broken)),
            "/s: string is not valid UTF-8"
        );
    }

    struct Oracle(::protobuf::reflect::MessageDescriptor);

    impl Oracle {
        fn new(text: &str, message: &str) -> Oracle {
            let file =
                ::protobuf::reflect::FileDescriptor::new_dynamic(run_parser(text).unwrap(), &[])
                    .unwrap();
            Oracle(file.message_by_package_relative_name(message).unwrap())
        }

        fn accepts(&self, bytes: &[u8]) -> bool {
            self.0.parse_from_bytes(bytes).is_ok()
        }
    }

    #[test]
    fn mutated_documents_never_panic_and_are_never_accepted_when_the_library_refuses() {
        let mut rng = Xorshift(0x2545_f491_4f6c_dd1d);
        for (format, schema, message) in [
            ("protobuf", "order.proto", "Order"),
            ("protobuf/proto2", "legacy.proto", "Legacy"),
        ] {
            let text = fixtures::read(format, schema);
            let compiled = compile(&text);
            let oracle = Oracle::new(&text, message);
            let seeds: Vec<Vec<u8>> = fixtures::cases(format, "datum")
                .into_iter()
                .filter(|c| c.expect.is_none() && !c.payload.is_empty())
                .map(|c| c.payload)
                .collect();
            assert!(!seeds.is_empty());
            let (mut accepted, mut refused) = (0, 0);
            for _ in 0..8000 {
                let mut bytes = seeds[rng.below(seeds.len())].clone();
                for _ in 0..1 + rng.below(4) {
                    let at = rng.below(bytes.len().max(1));
                    match rng.below(5) {
                        0 if !bytes.is_empty() => {
                            let index = at.min(bytes.len() - 1);
                            bytes[index] ^= 1 << rng.below(8);
                        }
                        1 if !bytes.is_empty() => {
                            let end = (at + 1 + rng.below(4)).min(bytes.len());
                            bytes.drain(at.min(bytes.len())..end);
                        }
                        2 => {
                            let junk: Vec<u8> =
                                (0..1 + rng.below(6)).map(|_| rng.next() as u8).collect();
                            bytes.splice(at.min(bytes.len())..at.min(bytes.len()), junk);
                        }
                        3 if !bytes.is_empty() => bytes.truncate(at),
                        _ if !bytes.is_empty() => {
                            let chunk = bytes[at.min(bytes.len() - 1)..]
                                .iter()
                                .take(8)
                                .copied()
                                .collect::<Vec<_>>();
                            bytes.extend(chunk);
                        }
                        _ => {}
                    }
                }
                match check(&compiled, &bytes) {
                    Ok(()) => {
                        accepted += 1;
                        assert!(
                            oracle.accepts(&bytes),
                            "{schema}: we accept what the library refuses: {bytes:02x?}"
                        );
                    }
                    Err(SchemaError::Violation(_)) => refused += 1,
                    Err(other) => panic!("{other:?} for {bytes:02x?}"),
                }
            }
            eprintln!("fuzz {schema}: {accepted} accepted, {refused} refused");
            assert!(
                accepted > 100 && refused > 100,
                "{schema}: {accepted}/{refused}"
            );
        }
    }
}
