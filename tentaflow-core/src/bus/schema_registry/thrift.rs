// =============================================================================
// File: bus/schema_registry/thrift.rs — Apache Thrift schemas (F4 B3)
// =============================================================================
// SUM/tentabus/PLAN-F4-REST.md §B.3. No library fits: the `thrift` crate is a
// runtime for generated code and ships no IDL parser, so the IDL subset below
// is read by `idl.rs` (a hand-written lexer and parser, no new dependency) and
// payloads are walked by `walker.rs` (one iterative decoder for both wire
// protocols). Both run on the representation defined here, lowered from the
// text once, at registration time.
//
// IDL accepted (ONE self-contained file):
//   - `struct`, `union`, `exception` (all three are structured types with
//     numbered fields), `enum` (explicit and implicit values), `typedef`,
//     `namespace` (read and ignored), `const` and field default values
//     (read for syntax within bounds, never type-checked, never carried to a
//     derived schema: neither changes a byte on the wire);
//   - base types `bool byte i8 i16 i32 i64 double string binary uuid`
//     (`i8` and `byte` are one type), containers `list<T> set<T> map<K, V>` nested up to 32
//     levels, references to any declared type (forward references too);
//   - field ids written out (`1: string name`), `required` / `optional` / the
//     default requiredness, `//` `#` and `/* */` comments, `;` or `,` between
//     members, annotations `( key = "value", ... )` after a type, a field, an
//     enum value, a typedef or a declaration (read and ignored, bounded).
// Refused with a fixed phrase: `include`, `cpp_include`, `service` (and
// `extends`, `oneway`, `throws`, `void`), `senum`, `slist`, `xsd_all` and the
// other `xsd_*` options, `cpp_type`, any other statement, a field without an
// explicit id (implicit negative ids are a compiler convention this registry
// does not reproduce), ids outside 1..32767, `required` in a union, a type
// name with a dot (it points into an included file), a reserved word as a
// name, a duplicate type, field id, field name or enum value name, a typedef
// that reaches itself, a root that can never produce a value (it requires
// itself).
//
// The root type (what every payload of the subject is an instance of) is the
// FIRST top-level struct, union or exception of the file, or the one named by
// the comment `// tentabus:struct <Name>` (`#` comments work too). The wire
// protocol is TBinaryProtocol, or TCompactProtocol with the comment
// `// tentabus:protocol compact` (`binary` may be written out). Both
// directives need no change to the register request and survive the derived
// projection.
//
// Wire format accepted by `validate`: ONE serialized root struct per record
// payload — no TMessage envelope, no framing, no trailing bytes. Strict where
// the schema says so: a declared field must arrive with its type's wire type,
// a `required` field must be present, a union carries at most one member (an
// EMPTY union is accepted; two members, or one twice, are a violation), an
// enum value must be one of the declared numbers (a deliberate strictness:
// Thrift runtimes keep unknown numbers, this registry reports them), a
// `string` is UTF-8 (a `binary` is not checked), a binary-protocol `bool` is 0
// or 1, a compact list or set of `bool` holds 1 (true) or 2 (false), lengths
// and counts are not negative and fit the bytes that remain, the compact
// varints are at most 3 / 5 / 10 bytes for i16 / i32 / i64 and do not
// overflow, a compact field id fits an i16. Lenient where Thrift is: a field
// id the schema does not declare is skipped (that is how versions evolve; the
// walk still checks its framing and depth), a singular field may repeat (the
// last wins), the element types of an EMPTY container are not compared with
// the schema. An unknown field of an invalid wire type is a violation.
//
// Resource bounds are structural, as in `avro.rs` and `protobuf.rs`:
//   - the parser is total (no `unwrap`, no unchecked indexing) and bounds
//     every dimension BEFORE it allocates: text size (`MAX_SCHEMA_TEXT_BYTES`),
//     tokens, container nesting in a type (32), nesting of a constant (16) and
//     its size, types (256), type expressions (16 384), fields per struct
//     (1024) and in total (4096), enum values per enum (1024) and in total
//     (4096), `required` fields per struct (64), identifiers (128 bytes, 8
//     dotted parts), string literals (256 bytes, no control, zero-width or
//     bidi characters), annotations, constants, namespaces, the typedef chain
//     (32). Comments are free text and may hold anything.
//   - types live in an arena of expressions; a typedef is a NAME in it, never
//     a copy, so a chain of typedefs that doubles at every step stays linear.
//   - `validate` is an iterative walk with an explicit stack of at most
//     `MAX_VALUE_DEPTH` frames (no recursion) and NO per-value allocation.
//     Every length and count is compared with the bytes left (a count also
//     with its minimum element size) BEFORE anything is skipped or iterated;
//     every field and every element costs a unit of a `Budget` (a document
//     cap shared by a publish batch through `ValidationBudget`). Running out
//     is `LimitExceeded`, never a verdict.
//   - `check_compatibility` runs under its own `Budget` with a depth cap and a
//     memo of visited pairs; what it cannot decide in budget is
//     `LimitExceeded`, never "compatible".
//
// Compatibility (`reads(writer, reader)`: everything written under `writer`
// can be read under `reader`; BACKWARD = `reads(old, new)`, FORWARD =
// `reads(new, old)`, FULL = both). A field's identity is its ID; names do not
// matter on the wire. A field on both sides keeps its type: base types must be
// equal (the one exception: a `string` can be read as `binary`, never the
// other way, because a `string` is checked for UTF-8), enum stays enum (the
// reader must declare every number the writer declares), struct stays the
// same KIND of struct (struct, union and exception do not mix), list stays
// list, set stays set, map stays map, compared element by element. A field
// only the writer has is skipped by the reader; a field only the reader has
// is absent from the data — fine, EXCEPT a `required` reader field, which the
// writer must also mark `required`. So adding a `required` field breaks
// BACKWARD, removing one breaks FORWARD.
//
// `derive_subschema` keeps the root's allowed top-level fields (by name) and
// every type they reach, and writes canonical IDL (`byte` is spelled `i8`, as the compiler recommends): the root first, then the
// reached types in declaration order, ids written out, defaults, constants
// and annotations dropped. A removed field simply is not listed; an allowed
// `required` field stays `required`. The text is checked to compile again.
//
// Violation messages are PATH + CONSTRAINT only (path = field names from the
// root, `[]` for an element, `<key>` / `<value>` for a map entry,
// `<unknown>` for a field the schema does not declare, `<root>` for the root
// itself) — never a payload value or a field id found in a payload; they
// reach audit rows, warn logs and DLQ headers.
// =============================================================================

use std::collections::{BTreeSet, HashSet};
use std::fmt::Write as _;

use super::{
    Budget, Compatibility, CompiledSchema, LimitExceeded, SchemaError, SchemaKindOps,
    ValidationBudget, MAX_SCHEMA_TEXT_BYTES,
};

mod idl;
mod walker;

#[cfg(test)]
mod tests;

const MAX_TOKENS: usize = 200_000;
const MAX_TYPE_NESTING: usize = 32;
/// Structs, unions, exceptions, enums and typedefs together.
const MAX_TYPES: usize = 256;
const MAX_EXPRS: usize = 16_384;
const MAX_FIELDS_PER_STRUCT: usize = 1024;
const MAX_FIELDS: usize = 4096;
const MAX_ENUM_VALUES_PER_ENUM: usize = 1024;
const MAX_ENUM_VALUES: usize = 4096;
const MAX_REQUIRED_FIELDS: usize = 64;
const MAX_CONSTS: usize = 256;
const MAX_NAMESPACES: usize = 64;
/// `key = "value"` pairs of all annotations together, and of one annotation.
const MAX_ANNOTATION_PAIRS: usize = 1024;
const MAX_ANNOTATION_PAIRS_EACH: usize = 16;
const MAX_VALUE_NESTING: usize = 16;
/// Tokens of all constants and defaults together.
const MAX_VALUE_NODES: usize = 16_384;
const MAX_TYPEDEF_CHAIN: usize = 32;
const MAX_NAME_LEN: usize = 128;
const MAX_NAME_PARTS: usize = 8;
const MAX_STRING_LITERAL: usize = 256;
const MAX_FIELD_ID: i64 = 32_767;
/// What a derived projection may weigh: enum values grow when spelled out
/// in canonical form.
const MAX_DERIVED_TEXT_BYTES: usize = 2 * MAX_SCHEMA_TEXT_BYTES;
const STRUCT_DIRECTIVE: &str = "tentabus:struct";
const PROTOCOL_DIRECTIVE: &str = "tentabus:protocol";

/// Nesting of structs and containers a payload may reach. Recursive schemas
/// make the depth data-dependent, so it is capped at check time.
const MAX_VALUE_DEPTH: usize = 64;
/// Work units one document may spend: about 5 ns each, so ~0.1 s at most.
const MAX_VALIDATION_UNITS: u64 = 20_000_000;
const MAX_COMPAT_WORK: u64 = 1_000_000;
const MAX_COMPAT_DEPTH: usize = 64;
/// Longest type spelled out in a message.
const MAX_LABEL_LEN: usize = 80;

const TOO_COMPLEX: &str = "the schemas are too complex to compare; compatibility cannot be proven";

fn invalid(msg: impl Into<String>) -> SchemaError {
    SchemaError::Invalid(msg.into())
}

// =============================================================================
// Lowered schema
// =============================================================================

pub(super) type ExprId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Base {
    Bool,
    Byte,
    I16,
    I32,
    I64,
    Double,
    String,
    Binary,
    Uuid,
}

impl Base {
    fn parse(word: &str) -> Option<Base> {
        Some(match word {
            "bool" => Base::Bool,
            "byte" | "i8" => Base::Byte,
            "i16" => Base::I16,
            "i32" => Base::I32,
            "i64" => Base::I64,
            "double" => Base::Double,
            "string" => Base::String,
            "binary" => Base::Binary,
            "uuid" => Base::Uuid,
            _ => return None,
        })
    }

    fn name(self) -> &'static str {
        match self {
            Base::Bool => "bool",
            Base::Byte => "i8",
            Base::I16 => "i16",
            Base::I32 => "i32",
            Base::I64 => "i64",
            Base::Double => "double",
            Base::String => "string",
            Base::Binary => "binary",
            Base::Uuid => "uuid",
        }
    }
}

/// A type as written. `Ref` exists only while the text is being parsed: the
/// resolution replaces it by the declaration it names.
#[derive(Debug)]
pub(super) enum Expr {
    Base(Base),
    Ref(String),
    Struct(usize),
    Enum(usize),
    Typedef(usize),
    List(ExprId),
    Set(ExprId),
    Map(ExprId, ExprId),
}

/// A type with every typedef followed: what the wire sees.
#[derive(Clone, Copy, Debug)]
pub(super) enum Shape {
    Base(Base),
    Struct(usize),
    Enum(usize),
    List(ExprId),
    Set(ExprId),
    Map(ExprId, ExprId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Struct,
    Union,
    Exception,
}

impl Kind {
    fn keyword(self) -> &'static str {
        match self {
            Kind::Struct => "struct",
            Kind::Union => "union",
            Kind::Exception => "exception",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Req {
    Required,
    Optional,
    Default,
}

#[derive(Debug)]
pub(super) struct Field {
    id: i16,
    name: String,
    req: Req,
    ty: ExprId,
    /// Position among the struct's `required` fields (at most 64).
    required_bit: Option<u32>,
}

#[derive(Debug)]
pub(super) struct StructDef {
    name: String,
    kind: Kind,
    fields: Vec<Field>,
    /// `(id, index into fields)` sorted by id.
    by_id: Vec<(i16, usize)>,
    required_mask: u64,
}

impl StructDef {
    fn field_by_id(&self, id: i32) -> Option<usize> {
        self.by_id
            .binary_search_by_key(&id, |(i, _)| i32::from(*i))
            .ok()
            .map(|at| self.by_id[at].1)
    }
}

#[derive(Debug)]
pub(super) struct EnumDef {
    name: String,
    values: Vec<(String, i32)>,
    /// Declared numbers, sorted and unique.
    numbers: Vec<i32>,
}

#[derive(Debug)]
pub(super) struct Typedef {
    name: String,
    target: ExprId,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum DeclRef {
    Struct(usize),
    Enum(usize),
    Typedef(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Protocol {
    Binary,
    Compact,
}

/// Registration-time form of a schema, consumed by `validate`.
#[derive(Debug)]
pub struct Compiled {
    structs: Vec<StructDef>,
    enums: Vec<EnumDef>,
    typedefs: Vec<Typedef>,
    exprs: Vec<Expr>,
    /// `shapes[e]` is `exprs[e]` with typedefs followed.
    shapes: Vec<Shape>,
    /// Declarations in the order the text wrote them.
    order: Vec<DeclRef>,
    root: usize,
    protocol: Protocol,
}

impl Compiled {
    /// `e` as the author wrote it (typedef names stay), for derived text.
    fn write_expr(&self, e: ExprId, out: &mut String) {
        match &self.exprs[e] {
            Expr::Base(b) => out.push_str(b.name()),
            Expr::Ref(name) => out.push_str(name),
            Expr::Struct(s) => out.push_str(&self.structs[*s].name),
            Expr::Enum(i) => out.push_str(&self.enums[*i].name),
            Expr::Typedef(t) => out.push_str(&self.typedefs[*t].name),
            Expr::List(c) => {
                out.push_str("list<");
                self.write_expr(*c, out);
                out.push('>');
            }
            Expr::Set(c) => {
                out.push_str("set<");
                self.write_expr(*c, out);
                out.push('>');
            }
            Expr::Map(k, v) => {
                out.push_str("map<");
                self.write_expr(*k, out);
                out.push_str(", ");
                self.write_expr(*v, out);
                out.push('>');
            }
        }
    }

    /// The type behind `e` for a message: typedefs followed, bounded in
    /// length and depth whatever the schema nests.
    fn label(&self, e: ExprId) -> String {
        let mut out = String::new();
        self.write_label(e, &mut out, 0);
        out
    }

    fn write_label(&self, e: ExprId, out: &mut String, depth: usize) {
        if out.len() >= MAX_LABEL_LEN || depth > MAX_COMPAT_DEPTH {
            if !out.ends_with('…') {
                out.push('…');
            }
            return;
        }
        match self.shapes[e] {
            Shape::Base(b) => out.push_str(b.name()),
            Shape::Struct(s) => {
                let def = &self.structs[s];
                let _ = write!(out, "{} {}", def.kind.keyword(), def.name);
            }
            Shape::Enum(i) => {
                let _ = write!(out, "enum {}", self.enums[i].name);
            }
            Shape::List(c) | Shape::Set(c) => {
                out.push_str(if matches!(self.shapes[e], Shape::List(_)) {
                    "list<"
                } else {
                    "set<"
                });
                self.write_label(c, out, depth + 1);
                out.push('>');
            }
            Shape::Map(k, v) => {
                out.push_str("map<");
                self.write_label(k, out, depth + 1);
                out.push_str(", ");
                self.write_label(v, out, depth + 1);
                out.push('>');
            }
        }
    }
}

fn parse_text(schema_text: &str) -> Result<Compiled, SchemaError> {
    parse_text_within(schema_text, MAX_SCHEMA_TEXT_BYTES)
}

fn parse_text_within(schema_text: &str, limit: usize) -> Result<Compiled, SchemaError> {
    if schema_text.trim().is_empty() {
        return Err(invalid("schema text is empty"));
    }
    if schema_text.len() > limit {
        return Err(invalid(format!(
            "schema text is {} bytes; the limit is {limit}",
            schema_text.len()
        )));
    }
    idl::parse(schema_text)
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
    seen_structs: HashSet<(usize, usize)>,
    seen_types: HashSet<(ExprId, ExprId)>,
}

fn show(path: &str) -> &str {
    if path.is_empty() {
        "<root>"
    } else {
        path
    }
}

impl Compat<'_> {
    fn labels(&self) -> (&'static str, &'static str) {
        if self.old_is_writer {
            ("old", "new")
        } else {
            ("new", "old")
        }
    }

    fn mismatch(&self, path: &str, we: ExprId, re: ExprId) -> Stop {
        let (wl, rl) = self.labels();
        Stop::No(format!(
            "{}: {} written by the {wl} schema cannot be read as {} by the {rl} schema",
            show(path),
            self.writer.label(we),
            self.reader.label(re),
        ))
    }

    fn structs(&mut self, w: usize, r: usize, path: &mut String, depth: usize) -> Result<(), Stop> {
        if !self.seen_structs.insert((w, r)) {
            return Ok(());
        }
        if depth > MAX_COMPAT_DEPTH {
            return Err(Stop::Complex);
        }
        let (wl, rl) = self.labels();
        let (writer, reader) = (self.writer, self.reader);
        let (ws, rs) = (&writer.structs[w], &reader.structs[r]);
        if ws.kind != rs.kind {
            return Err(Stop::No(format!(
                "{}: {} '{}' written by the {wl} schema cannot be read as {} '{}' by the {rl} schema",
                show(path),
                ws.kind.keyword(),
                ws.name,
                rs.kind.keyword(),
                rs.name,
            )));
        }
        for (id, r_index) in &rs.by_id {
            self.budget.charge(1)?;
            let rf = &rs.fields[*r_index];
            let Some(w_index) = ws.field_by_id(i32::from(*id)) else {
                if rf.req == Req::Required {
                    return Err(Stop::No(format!(
                        "{path}/{}: the {rl} schema requires field '{}' (id {id}) of {} '{}', which the {wl} schema does not always write",
                        rf.name,
                        rf.name,
                        rs.kind.keyword(),
                        rs.name
                    )));
                }
                continue;
            };
            let wf = &ws.fields[w_index];
            if rf.req == Req::Required && wf.req != Req::Required {
                return Err(Stop::No(format!(
                    "{path}/{}: the {rl} schema requires field '{}' (id {id}) of {} '{}', which the {wl} schema does not always write",
                    rf.name,
                    rf.name,
                    rs.kind.keyword(),
                    rs.name
                )));
            }
            let keep = path.len();
            path.push('/');
            path.push_str(&rf.name);
            let verdict = self.types(wf.ty, rf.ty, path, depth + 1);
            path.truncate(keep);
            verdict?;
        }
        Ok(())
    }

    fn types(
        &mut self,
        we: ExprId,
        re: ExprId,
        path: &mut String,
        depth: usize,
    ) -> Result<(), Stop> {
        self.budget.charge(1)?;
        if !self.seen_types.insert((we, re)) {
            return Ok(());
        }
        if depth > MAX_COMPAT_DEPTH {
            return Err(Stop::Complex);
        }
        let (writer, reader) = (self.writer, self.reader);
        let step = |this: &mut Self,
                    path: &mut String,
                    segment: &str,
                    w: ExprId,
                    r: ExprId|
         -> Result<(), Stop> {
            let keep = path.len();
            path.push_str(segment);
            let verdict = this.types(w, r, path, depth + 1);
            path.truncate(keep);
            verdict
        };
        match (writer.shapes[we], reader.shapes[re]) {
            (Shape::Base(a), Shape::Base(b)) => {
                if a == b || (a == Base::String && b == Base::Binary) {
                    Ok(())
                } else {
                    Err(self.mismatch(path, we, re))
                }
            }
            (Shape::Enum(a), Shape::Enum(b)) => self.enums(a, b, path),
            (Shape::Struct(a), Shape::Struct(b)) => self.structs(a, b, path, depth),
            (Shape::List(a), Shape::List(b)) | (Shape::Set(a), Shape::Set(b)) => {
                step(self, path, "/[]", a, b)
            }
            (Shape::Map(ka, va), Shape::Map(kb, vb)) => {
                step(self, path, "/<key>", ka, kb)?;
                step(self, path, "/<value>", va, vb)
            }
            _ => Err(self.mismatch(path, we, re)),
        }
    }

    /// A reader enum is closed: it needs every number the writer declares.
    fn enums(&mut self, w: usize, r: usize, path: &str) -> Result<(), Stop> {
        let (wl, rl) = self.labels();
        let (we, re) = (&self.writer.enums[w], &self.reader.enums[r]);
        self.budget.charge(we.numbers.len() as u64)?;
        match we
            .numbers
            .iter()
            .find(|n| re.numbers.binary_search(n).is_err())
        {
            Some(value) => Err(Stop::No(format!(
                "{}: enum '{}': value {value} of the {wl} schema is unknown to the {rl} schema",
                show(path),
                re.name
            ))),
            None => Ok(()),
        }
    }
}

/// `Ok` when everything written under `writer` can be read under `reader`,
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
        seen_structs: HashSet::new(),
        seen_types: HashSet::new(),
    };
    compat.structs(writer.root, reader.root, &mut String::new(), 0)
}

// =============================================================================
// Projection
// =============================================================================

/// Declarations a projection writes.
struct Keep {
    structs: HashSet<usize>,
    enums: HashSet<usize>,
    typedefs: HashSet<usize>,
}

fn collect_keep(c: &Compiled, allowed: &BTreeSet<String>) -> Keep {
    let mut keep = Keep {
        structs: HashSet::from([c.root]),
        enums: HashSet::new(),
        typedefs: HashSet::new(),
    };
    let mut seen: HashSet<ExprId> = HashSet::new();
    let mut work: Vec<ExprId> = c.structs[c.root]
        .fields
        .iter()
        .filter(|f| allowed.contains(&f.name))
        .map(|f| f.ty)
        .collect();
    while let Some(e) = work.pop() {
        if !seen.insert(e) {
            continue;
        }
        match &c.exprs[e] {
            Expr::Base(_) | Expr::Ref(_) => {}
            Expr::Enum(i) => {
                keep.enums.insert(*i);
            }
            Expr::Struct(s) => {
                if keep.structs.insert(*s) {
                    work.extend(c.structs[*s].fields.iter().map(|f| f.ty));
                }
            }
            Expr::Typedef(t) => {
                if keep.typedefs.insert(*t) {
                    work.push(c.typedefs[*t].target);
                }
            }
            Expr::List(child) | Expr::Set(child) => work.push(*child),
            Expr::Map(k, v) => work.extend([*k, *v]),
        }
    }
    keep
}

fn write_struct(c: &Compiled, s: usize, allowed: Option<&BTreeSet<String>>, out: &mut String) {
    let def = &c.structs[s];
    let _ = writeln!(out, "{} {} {{", def.kind.keyword(), def.name);
    for field in &def.fields {
        if allowed.is_some_and(|names| !names.contains(&field.name)) {
            continue;
        }
        let req = match field.req {
            Req::Required => "required ",
            Req::Optional => "optional ",
            Req::Default => "",
        };
        let mut ty = String::new();
        c.write_expr(field.ty, &mut ty);
        let _ = writeln!(out, "  {}: {req}{ty} {},", field.id, field.name);
    }
    out.push_str("}\n");
}

fn derive(c: &Compiled, allowed: &BTreeSet<String>) -> String {
    let keep = collect_keep(c, allowed);
    let mut out = String::new();
    if c.protocol == Protocol::Compact {
        let _ = writeln!(out, "// {PROTOCOL_DIRECTIVE} compact");
    }
    write_struct(c, c.root, Some(allowed), &mut out);
    for decl in &c.order {
        out.push('\n');
        match *decl {
            DeclRef::Struct(s) if s != c.root && keep.structs.contains(&s) => {
                write_struct(c, s, None, &mut out);
            }
            DeclRef::Enum(i) if keep.enums.contains(&i) => {
                let def = &c.enums[i];
                let _ = writeln!(out, "enum {} {{", def.name);
                for (name, number) in &def.values {
                    let _ = writeln!(out, "  {name} = {number},");
                }
                out.push_str("}\n");
            }
            DeclRef::Typedef(t) if keep.typedefs.contains(&t) => {
                let def = &c.typedefs[t];
                let mut ty = String::new();
                c.write_expr(def.target, &mut ty);
                let _ = writeln!(out, "typedef {ty} {}", def.name);
            }
            _ => {
                // Nothing written for this declaration: take back its blank line.
                out.pop();
            }
        }
    }
    out
}

// =============================================================================
// SchemaKindOps
// =============================================================================

pub struct ThriftOps;

impl SchemaKindOps for ThriftOps {
    fn compile(&self, schema_text: &str) -> Result<CompiledSchema, SchemaError> {
        parse_text(schema_text).map(CompiledSchema::Thrift)
    }

    fn validate_metered(
        &self,
        compiled: &CompiledSchema,
        payload: &[u8],
        shared: &mut ValidationBudget,
    ) -> Result<(), SchemaError> {
        let CompiledSchema::Thrift(c) = compiled else {
            return Err(invalid("validate called with a non-thrift compiled schema"));
        };
        let mut budget = Budget::new(shared.allowance(MAX_VALIDATION_UNITS));
        let verdict = walker::validate_with(c, payload, &mut budget);
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
        // The projection must itself compile. Enum values grow when
        // written out, so the check allows twice the source limit, not more:
        // the caps on names, fields and values bound the text built here.
        parse_text_within(&derived, MAX_DERIVED_TEXT_BYTES)?;
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

pub(super) static THRIFT_OPS: ThriftOps = ThriftOps;
