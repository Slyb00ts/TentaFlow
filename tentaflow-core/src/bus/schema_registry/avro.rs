// =============================================================================
// File: bus/schema_registry/avro.rs — Apache Avro schemas (F4 B1)
// =============================================================================
// SUM/tentabus/PLAN-F4-REST.md §B.1. `apache-avro` (ASF, no codec features)
// parses and serializes SCHEMAS; everything that touches untrusted bytes or
// decides compatibility runs on a small intermediate representation lowered
// from the parsed schema, because the library's own readers are not safe to
// put on the publish path:
//   - `from_avro_datum` allocates what the datum DECLARES (bounded only by a
//     process-global, set-once limit whose default is 512 MiB), allocates
//     `vec![0; size]` for a schema-declared `fixed`, and treats a payload that
//     ends inside a boolean or a union index as a valid null.
//   - `SchemaCompatibility::can_read` compares a `Ref` with an inline
//     definition as a type mismatch, so moving a named type's definition
//     between two fields reads as an incompatible change, and answers
//     `Partial` for "some data would fail", which registry semantics must
//     refuse.
//
// Wire format accepted by `validate`: ONE raw Avro binary datum per record
// payload, encoded with the registered schema as the writer schema. NOT
// supported (and so refused, not guessed at): Object Container Files
// (`Obj\x01` header), the Avro single-object encoding (`C3 01` + fingerprint)
// and the Confluent wire format (`00` + 4-byte schema id). A payload with any
// bytes left after the datum is a violation. Logical types are checked at
// their underlying type only (a `uuid` string is not parsed as a UUID).
//
// Resource bounds are structural, as in `xsd.rs`:
//   - `compile` caps the text (`MAX_SCHEMA_TEXT_BYTES`), JSON nesting (the
//     parser's recursion limit), types (`MAX_SCHEMA_NODES`), schema depth,
//     fields per record, union branches, enum symbols and `fixed` size, and
//     refuses a schema that can have no value at all (a record that contains
//     itself without an optional branch).
//   - `validate` walks the datum against the lowered schema with NO per-value
//     allocation. A declared length is checked against the bytes left BEFORE
//     anything is read; an array or map block must be able to fit
//     `count x minimum encoded size` bytes (so a count bomb is a violation at
//     once); zero-width items (`null`, empty records) are charged for in one
//     step. Every value costs a unit of a `Budget` (a document cap shared by
//     a publish batch through `ValidationBudget`), nesting is capped at
//     `MAX_VALUE_DEPTH`, and running out is `LimitExceeded`, never a verdict.
//     There is no per-process gate: a check allocates nothing proportional to
//     the payload, so concurrency is bounded by the CPU budget alone.
//   - `check_compatibility` runs on the same representation under its own
//     `Budget` (`MAX_COMPAT_WORK`) with a depth cap and a bounded memo;
//     what it cannot decide in budget is `LimitExceeded`, never "compatible".
//
// Compatibility semantics (Avro schema resolution, Java `SchemaCompatibility`
// strictness): `reads(writer, reader)` holds when every datum written under
// `writer` can be read under `reader`. BACKWARD = `reads(old, new)` (a reader
// on the NEW schema reads data written under the OLD), FORWARD =
// `reads(new, old)`, FULL = both. A writer union needs EVERY branch readable;
// an enum needs every writer symbol known to the reader or a reader default;
// a reader field absent from the writer needs a default.
//
// `derive_subschema` keeps the record's allowed top-level fields and
// re-serializes through the library, hoisting the definition of a named type
// into the first kept place that mentions it (a `Ref` must never outlive its
// definition). The result is checked to compile.
//
// Violation messages are PATH + CONSTRAINT only (path = record field names,
// `[]` for array items, `{}` for map values) — never a payload value; they
// reach audit rows, warn logs and DLQ headers.
// =============================================================================

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use apache_avro::schema::{
    Alias, ArraySchema, FixedSchema, InnerDecimalSchema, MapSchema, RecordField, RecordSchema,
    Schema, UnionSchema, UuidSchema,
};
use serde_json::Value;

use super::{
    shorten_path, Budget, Compatibility, CompiledSchema, LimitExceeded, SchemaError, SchemaKindOps,
    ValidationBudget, MAX_SCHEMA_TEXT_BYTES,
};

const MAX_SCHEMA_NODES: usize = 4096;
const MAX_SCHEMA_DEPTH: usize = 64;
const MAX_RECORD_FIELDS: usize = 1024;
const MAX_UNION_BRANCHES: usize = 64;
const MAX_ENUM_SYMBOLS: usize = 1024;
const MAX_FIXED_SIZE: usize = 16 * 1024 * 1024;

/// Nesting of containers (records, arrays, maps; a union adds none) a datum
/// may reach. Recursive schemas make the depth data-dependent, so it is capped
/// at check time; `a_deep_datum_needs_little_stack` pins that the walk fits a
/// small stack even in a debug build.
const MAX_VALUE_DEPTH: usize = 64;
/// Work units one document may spend: about 5 ns each, so ~0.1 s at most.
const MAX_VALIDATION_UNITS: u64 = 20_000_000;
const MAX_COMPAT_WORK: u64 = 1_000_000;
const MAX_COMPAT_DEPTH: usize = 128;
const MAX_COMPAT_MEMO: usize = 65_536;

const INFINITE: u64 = u64::MAX;

const TRUNCATED: &str = "payload ends before the value is complete";
const INT_TOO_LONG: &str = "integer encoding is longer than 10 bytes or overflows 64 bits";
const INT_RANGE: &str = "value does not fit the int type";
const BAD_BOOLEAN: &str = "boolean is neither 0 nor 1";
const NEGATIVE_LENGTH: &str = "length is negative";
const LENGTH_EXCEEDS: &str = "declared length exceeds the remaining payload";
const BAD_UTF8: &str = "string is not valid UTF-8";
const BAD_ENUM: &str = "enum index is out of range";
const BAD_UNION: &str = "union branch index is out of range";
const BLOCK_COUNT: &str = "block declares more items than the remaining payload can hold";
const BLOCK_SIZE: &str = "block byte size does not match its items";
const TRAILING: &str = "bytes remain after the value";
const TOO_DEEP: &str = "value is nested too deeply to check";
const VALIDATION_BUDGET: &str = "document exceeds the validation work budget";
const TOO_COMPLEX: &str = "the schemas are too complex to compare; compatibility cannot be proven";

fn invalid(msg: impl Into<String>) -> SchemaError {
    SchemaError::Invalid(msg.into())
}

// =============================================================================
// Lowered schema
// =============================================================================

type NodeId = usize;

#[derive(Debug)]
struct Node {
    kind: Kind,
    /// Fewest bytes any value of this type encodes to; `INFINITE` when no
    /// finite value exists.
    min_bytes: u64,
}

#[derive(Debug)]
enum Kind {
    Null,
    Boolean,
    Int,
    Long,
    Float,
    Double,
    Bytes {
        decimal: Option<(usize, usize)>,
    },
    String,
    Fixed {
        name: String,
        aliases: Vec<String>,
        size: usize,
        decimal: Option<(usize, usize)>,
    },
    Enum {
        name: String,
        aliases: Vec<String>,
        symbols: Vec<String>,
        has_default: bool,
    },
    Array(NodeId),
    Map(NodeId),
    Union(Vec<NodeId>),
    Record {
        name: String,
        aliases: Vec<String>,
        fields: Vec<Field>,
    },
}

#[derive(Debug)]
struct Field {
    name: String,
    aliases: Vec<String>,
    has_default: bool,
    node: NodeId,
}

#[derive(Debug)]
pub struct Compiled {
    nodes: Vec<Node>,
    root: NodeId,
}

struct Lowerer {
    nodes: Vec<Node>,
    /// Full name (and alias full names) of every named type seen so far.
    named: HashMap<String, NodeId>,
}

fn alias_names(aliases: &Option<Vec<Alias>>) -> Vec<String> {
    aliases
        .iter()
        .flatten()
        .map(|a| a.name().to_string())
        .collect()
}

impl Lowerer {
    fn push(&mut self, kind: Kind) -> Result<NodeId, SchemaError> {
        if self.nodes.len() >= MAX_SCHEMA_NODES {
            return Err(invalid(format!(
                "the schema has more than {MAX_SCHEMA_NODES} types"
            )));
        }
        self.nodes.push(Node {
            kind,
            min_bytes: INFINITE,
        });
        Ok(self.nodes.len() - 1)
    }

    /// Records the definition of a named type. The parser lets a name be
    /// defined twice; a reference would then mean whichever came first.
    fn register(
        &mut self,
        full_name: String,
        aliases: &Option<Vec<Alias>>,
        id: NodeId,
    ) -> Result<(), SchemaError> {
        if self.named.insert(full_name.clone(), id).is_some() {
            return Err(invalid(format!(
                "type '{full_name}' is defined more than once"
            )));
        }
        for alias in aliases.iter().flatten() {
            self.named.entry(alias.fullname(None)).or_insert(id);
        }
        Ok(())
    }

    fn fixed(
        &mut self,
        fixed: &FixedSchema,
        decimal: Option<(usize, usize)>,
    ) -> Result<NodeId, SchemaError> {
        if fixed.size > MAX_FIXED_SIZE {
            return Err(invalid(format!(
                "fixed '{}' is larger than {MAX_FIXED_SIZE} bytes",
                fixed.name.name()
            )));
        }
        let id = self.push(Kind::Fixed {
            name: fixed.name.name().to_string(),
            aliases: alias_names(&fixed.aliases),
            size: fixed.size,
            decimal,
        })?;
        self.register(fixed.name.fullname(None), &fixed.aliases, id)?;
        Ok(id)
    }

    fn lower(&mut self, schema: &Schema, depth: usize) -> Result<NodeId, SchemaError> {
        if depth > MAX_SCHEMA_DEPTH {
            return Err(invalid(format!(
                "the schema is nested deeper than {MAX_SCHEMA_DEPTH} levels"
            )));
        }
        match schema {
            Schema::Null => self.push(Kind::Null),
            Schema::Boolean => self.push(Kind::Boolean),
            Schema::Int | Schema::Date | Schema::TimeMillis => self.push(Kind::Int),
            Schema::Long
            | Schema::TimeMicros
            | Schema::TimestampMillis
            | Schema::TimestampMicros
            | Schema::TimestampNanos
            | Schema::LocalTimestampMillis
            | Schema::LocalTimestampMicros
            | Schema::LocalTimestampNanos => self.push(Kind::Long),
            Schema::Float => self.push(Kind::Float),
            Schema::Double => self.push(Kind::Double),
            Schema::Bytes | Schema::BigDecimal | Schema::Uuid(UuidSchema::Bytes) => {
                self.push(Kind::Bytes { decimal: None })
            }
            Schema::String | Schema::Uuid(UuidSchema::String) => self.push(Kind::String),
            Schema::Decimal(d) => match &d.inner {
                InnerDecimalSchema::Bytes => self.push(Kind::Bytes {
                    decimal: Some((d.precision, d.scale)),
                }),
                InnerDecimalSchema::Fixed(f) => self.fixed(f, Some((d.precision, d.scale))),
            },
            Schema::Uuid(UuidSchema::Fixed(f)) | Schema::Duration(f) | Schema::Fixed(f) => {
                self.fixed(f, None)
            }
            Schema::Enum(e) => {
                if e.symbols.len() > MAX_ENUM_SYMBOLS {
                    return Err(invalid(format!(
                        "enum '{}' has more than {MAX_ENUM_SYMBOLS} symbols",
                        e.name.name()
                    )));
                }
                let id = self.push(Kind::Enum {
                    name: e.name.name().to_string(),
                    aliases: alias_names(&e.aliases),
                    symbols: e.symbols.clone(),
                    has_default: e.default.is_some(),
                })?;
                self.register(e.name.fullname(None), &e.aliases, id)?;
                Ok(id)
            }
            Schema::Array(ArraySchema { items, .. }) => {
                let id = self.push(Kind::Array(0))?;
                let item = self.lower(items, depth + 1)?;
                self.nodes[id].kind = Kind::Array(item);
                Ok(id)
            }
            Schema::Map(MapSchema { types, .. }) => {
                let id = self.push(Kind::Map(0))?;
                let value = self.lower(types, depth + 1)?;
                self.nodes[id].kind = Kind::Map(value);
                Ok(id)
            }
            Schema::Union(u) => {
                let branches = u.variants();
                if branches.len() > MAX_UNION_BRANCHES {
                    return Err(invalid(format!(
                        "a union has more than {MAX_UNION_BRANCHES} branches"
                    )));
                }
                let id = self.push(Kind::Union(Vec::new()))?;
                let mut lowered = Vec::with_capacity(branches.len());
                for branch in branches {
                    lowered.push(self.lower(branch, depth + 1)?);
                }
                self.nodes[id].kind = Kind::Union(lowered);
                Ok(id)
            }
            Schema::Record(r) => {
                if r.fields.len() > MAX_RECORD_FIELDS {
                    return Err(invalid(format!(
                        "record '{}' has more than {MAX_RECORD_FIELDS} fields",
                        r.name.name()
                    )));
                }
                let id = self.push(Kind::Record {
                    name: r.name.name().to_string(),
                    aliases: alias_names(&r.aliases),
                    fields: Vec::new(),
                })?;
                self.register(r.name.fullname(None), &r.aliases, id)?;
                let mut fields = Vec::with_capacity(r.fields.len());
                for f in &r.fields {
                    if let (Some(default), Schema::Union(u)) = (&f.default, &f.schema) {
                        if !union_default_fits(u, default) {
                            return Err(invalid(format!(
                                "the default of field '{}' does not fit the first branch of its union",
                                f.name
                            )));
                        }
                    }
                    fields.push(Field {
                        name: f.name.clone(),
                        aliases: f.aliases.clone(),
                        has_default: f.default.is_some(),
                        node: self.lower(&f.schema, depth + 1)?,
                    });
                }
                if let Kind::Record { fields: slot, .. } = &mut self.nodes[id].kind {
                    *slot = fields;
                }
                Ok(id)
            }
            Schema::Ref { name } => {
                self.named
                    .get(&name.fullname(None))
                    .copied()
                    .ok_or_else(|| {
                        invalid(format!(
                            "type '{}' is used before it is defined",
                            name.fullname(None)
                        ))
                    })
            }
        }
    }
}

/// The parser checks a field default against its type except for a union,
/// where Avro uses the FIRST branch; a default of the wrong kind would only
/// surface when a reader fills the field. Named first branches are not
/// inspected.
fn union_default_fits(union: &UnionSchema, default: &Value) -> bool {
    match union.variants().first() {
        Some(Schema::Null) => default.is_null(),
        Some(Schema::Boolean) => default.is_boolean(),
        Some(Schema::Int | Schema::Long) => default.is_i64() || default.is_u64(),
        Some(Schema::Float | Schema::Double) => default.is_number(),
        Some(Schema::String | Schema::Bytes) => default.is_string(),
        Some(Schema::Array(_)) => default.is_array(),
        Some(Schema::Map(_)) => default.is_object(),
        _ => true,
    }
}

/// Smallest encoded size per type, as a fixpoint (types may refer to
/// themselves). A type left at `INFINITE` has no finite value.
fn compute_min_bytes(nodes: &mut [Node]) {
    for pass in 0..=nodes.len() {
        let mut changed = false;
        for i in (0..nodes.len()).rev() {
            let min = match &nodes[i].kind {
                Kind::Null => 0,
                Kind::Boolean | Kind::Int | Kind::Long | Kind::Bytes { .. } | Kind::String => 1,
                Kind::Enum { .. } | Kind::Array(_) | Kind::Map(_) => 1,
                Kind::Float => 4,
                Kind::Double => 8,
                Kind::Fixed { size, .. } => *size as u64,
                Kind::Union(branches) => branches
                    .iter()
                    .map(|b| nodes[*b].min_bytes)
                    .min()
                    .map_or(INFINITE, |m| m.saturating_add(1)),
                Kind::Record { fields, .. } => fields
                    .iter()
                    .fold(0u64, |sum, f| sum.saturating_add(nodes[f.node].min_bytes)),
            };
            if min < nodes[i].min_bytes {
                nodes[i].min_bytes = min;
                changed = true;
            }
        }
        if !changed || pass == nodes.len() {
            break;
        }
    }
}

/// The parser silently drops a record field that is not a JSON object, which
/// would turn a typo into a schema that checks less than its author wrote.
fn check_field_shapes(value: &Value) -> Result<(), SchemaError> {
    match value {
        Value::Array(branches) => branches.iter().try_for_each(check_field_shapes),
        Value::Object(map) => match map.get("type") {
            Some(Value::String(kind)) => match kind.as_str() {
                "record" | "error" => {
                    let Some(fields) = map.get("fields") else {
                        return Ok(());
                    };
                    let Some(fields) = fields.as_array() else {
                        return Err(invalid(
                            "a record lists its fields in something that is not an array",
                        ));
                    };
                    for field in fields {
                        let Some(field) = field.as_object() else {
                            return Err(invalid("a record lists a field that is not an object"));
                        };
                        if let Some(ty) = field.get("type") {
                            check_field_shapes(ty)?;
                        }
                    }
                    Ok(())
                }
                "array" => map.get("items").map_or(Ok(()), check_field_shapes),
                "map" => map.get("values").map_or(Ok(()), check_field_shapes),
                _ => Ok(()),
            },
            Some(nested @ (Value::Object(_) | Value::Array(_))) => check_field_shapes(nested),
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}

fn parse_text(schema_text: &str) -> Result<(Schema, Compiled), SchemaError> {
    if schema_text.len() > MAX_SCHEMA_TEXT_BYTES {
        return Err(invalid(format!(
            "schema text is {} bytes, exceeding the {MAX_SCHEMA_TEXT_BYTES}-byte limit",
            schema_text.len()
        )));
    }
    if schema_text.trim().is_empty() {
        return Err(invalid("schema text is empty"));
    }
    let value: Value = serde_json::from_str(schema_text)
        .map_err(|e| invalid(format!("the schema is not valid JSON: {e}")))?;
    check_field_shapes(&value)?;
    let schema =
        Schema::parse(&value).map_err(|e| invalid(format!("not a valid Avro schema: {e}")))?;
    drop(value);
    let mut lowerer = Lowerer {
        nodes: Vec::new(),
        named: HashMap::new(),
    };
    let root = lowerer.lower(&schema, 0)?;
    let mut nodes = lowerer.nodes;
    compute_min_bytes(&mut nodes);
    if nodes[root].min_bytes == INFINITE {
        return Err(invalid(
            "the schema can never produce a value: a record contains itself without an optional \
             branch (a union with null, an array or a map)",
        ));
    }
    Ok((schema, Compiled { nodes, root }))
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

enum Seg<'a> {
    Field(&'a str),
    Item,
    Entry,
}

fn render_path(path: &[Seg<'_>]) -> String {
    if path.is_empty() {
        return "<root>".to_string();
    }
    let mut out = String::new();
    for seg in path {
        out.push('/');
        match seg {
            Seg::Field(name) => out.push_str(name),
            Seg::Item => out.push_str("[]"),
            Seg::Entry => out.push_str("{}"),
        }
    }
    out
}

struct Walker<'a> {
    nodes: &'a [Node],
    data: &'a [u8],
    pos: usize,
    budget: &'a mut Budget,
    path: Vec<Seg<'a>>,
}

type Walk<T> = Result<T, Fail>;

impl<'a> Walker<'a> {
    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Walk<&'a [u8]> {
        if n > self.remaining() {
            return Err(Fail::Violation(TRUNCATED));
        }
        let bytes = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(bytes)
    }

    fn varint(&mut self) -> Walk<u64> {
        let mut value = 0u64;
        for i in 0..10 {
            let byte = *self.data.get(self.pos).ok_or(Fail::Violation(TRUNCATED))?;
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

    fn long(&mut self) -> Walk<i64> {
        let n = self.varint()?;
        Ok(((n >> 1) as i64) ^ -((n & 1) as i64))
    }

    /// A byte length that is non-negative and fits in what is left.
    fn length(&mut self) -> Walk<usize> {
        let len = self.long()?;
        if len < 0 {
            return Err(Fail::Violation(NEGATIVE_LENGTH));
        }
        if len as u64 > self.remaining() as u64 {
            return Err(Fail::Violation(LENGTH_EXCEEDS));
        }
        Ok(len as usize)
    }

    fn string(&mut self) -> Walk<()> {
        let len = self.length()?;
        self.budget.charge(len as u64 / 8)?;
        std::str::from_utf8(self.take(len)?).map_err(|_| Fail::Violation(BAD_UTF8))?;
        Ok(())
    }

    fn value(&mut self, id: NodeId, depth: usize) -> Walk<()> {
        if depth > MAX_VALUE_DEPTH {
            return Err(Fail::Limit(TOO_DEEP));
        }
        self.budget.charge(1)?;
        let nodes = self.nodes;
        match &nodes[id].kind {
            Kind::Null => {}
            Kind::Boolean => {
                if self.take(1)?[0] > 1 {
                    return Err(Fail::Violation(BAD_BOOLEAN));
                }
            }
            Kind::Int => {
                if i32::try_from(self.long()?).is_err() {
                    return Err(Fail::Violation(INT_RANGE));
                }
            }
            Kind::Long => {
                self.long()?;
            }
            Kind::Float => {
                self.take(4)?;
            }
            Kind::Double => {
                self.take(8)?;
            }
            Kind::Bytes { .. } => {
                let len = self.length()?;
                self.budget.charge(len as u64 / 8)?;
                self.take(len)?;
            }
            Kind::String => self.string()?,
            Kind::Fixed { size, .. } => {
                self.budget.charge(*size as u64 / 8)?;
                self.take(*size)?;
            }
            Kind::Enum { symbols, .. } => {
                let index = self.long()?;
                if index < 0 || index as u64 >= symbols.len() as u64 {
                    return Err(Fail::Violation(BAD_ENUM));
                }
            }
            Kind::Array(item) => self.blocks(*item, false, depth)?,
            Kind::Map(value) => self.blocks(*value, true, depth)?,
            Kind::Union(branches) => {
                let index = self.long()?;
                if index < 0 || index as u64 >= branches.len() as u64 {
                    return Err(Fail::Violation(BAD_UNION));
                }
                self.value(branches[index as usize], depth)?;
            }
            Kind::Record { fields, .. } => {
                for field in fields {
                    self.path.push(Seg::Field(&field.name));
                    self.value(field.node, depth + 1)?;
                    self.path.pop();
                }
            }
        }
        Ok(())
    }

    /// The blocks of an array or map. A negative count is followed by the
    /// block's byte size, which must match what the items consumed.
    fn blocks(&mut self, item: NodeId, is_map: bool, depth: usize) -> Walk<()> {
        let nodes = self.nodes;
        // A map entry is a key (a length, at least one byte) and a value.
        let per_item = nodes[item].min_bytes.saturating_add(u64::from(is_map));
        self.path.push(if is_map { Seg::Entry } else { Seg::Item });
        loop {
            let header = self.long()?;
            if header == 0 {
                break;
            }
            self.budget.charge(1)?;
            let count = header.unsigned_abs();
            let declared_size = if header < 0 {
                let size = self.long()?;
                if size < 0 || size as u64 > self.remaining() as u64 {
                    return Err(Fail::Violation(BLOCK_SIZE));
                }
                Some(size as u64)
            } else {
                None
            };
            if per_item > 0 && count.saturating_mul(per_item) > self.remaining() as u64 {
                return Err(Fail::Violation(BLOCK_COUNT));
            }
            if per_item == 0 && count > self.budget.remaining() {
                return Err(Fail::Limit(VALIDATION_BUDGET));
            }
            let start = self.pos;
            if !is_map && matches!(nodes[item].kind, Kind::Null) {
                self.budget.charge(count)?;
            } else {
                for _ in 0..count {
                    if is_map {
                        self.string()?;
                    }
                    self.value(item, depth + 1)?;
                }
            }
            if declared_size.is_some_and(|size| size != (self.pos - start) as u64) {
                return Err(Fail::Violation(BLOCK_SIZE));
            }
        }
        self.path.pop();
        Ok(())
    }
}

fn validate_with(c: &Compiled, payload: &[u8], budget: &mut Budget) -> Result<(), SchemaError> {
    let mut walker = Walker {
        nodes: &c.nodes,
        data: payload,
        pos: 0,
        budget,
        path: Vec::new(),
    };
    let outcome = walker.value(c.root, 0).and_then(|()| {
        if walker.pos == payload.len() {
            Ok(())
        } else {
            walker.path.clear();
            Err(Fail::Violation(TRAILING))
        }
    });
    match outcome {
        Ok(()) => Ok(()),
        Err(Fail::Violation(msg)) => Err(SchemaError::Violation(format!(
            "{}: {msg}",
            shorten_path(&render_path(&walker.path))
        ))),
        Err(Fail::Limit(msg)) => Err(SchemaError::LimitExceeded(format!(
            "{}: {msg}",
            shorten_path(&render_path(&walker.path))
        ))),
    }
}

// =============================================================================
// Compatibility
// =============================================================================

#[derive(Clone)]
enum Step {
    Field(String),
    Item,
    Entry,
}

#[derive(Clone)]
enum Msg {
    Type { writer: String, reader: String },
    Names { writer: String, reader: String },
    FixedSize { name: String },
    Decimal,
    EnumSymbol { name: String, symbol: String },
    MissingDefault { record: String, field: String },
    UnionWriter { branch: String },
    UnionReader { branch: String },
}

#[derive(Clone)]
struct Why {
    /// Innermost first.
    steps: Vec<Step>,
    msg: Msg,
}

impl Why {
    fn new(msg: Msg) -> Why {
        Why {
            steps: Vec::new(),
            msg,
        }
    }

    fn within(mut self, step: Step) -> Why {
        self.steps.push(step);
        self
    }

    /// `writer` / `reader` are the labels of the sides ("old" / "new").
    fn render(&self, writer: &str, reader: &str) -> String {
        let mut path = String::new();
        for step in self.steps.iter().rev() {
            path.push('/');
            match step {
                Step::Field(name) => path.push_str(name),
                Step::Item => path.push_str("[]"),
                Step::Entry => path.push_str("{}"),
            }
        }
        if path.is_empty() {
            path.push_str("<root>");
        }
        let text = match &self.msg {
            Msg::Type {
                writer: w,
                reader: r,
            } => format!(
                "{w} written by the {writer} schema cannot be read as {r} by the {reader} schema"
            ),
            Msg::Names {
                writer: w,
                reader: r,
            } => {
                format!("{w} cannot be read as {r}: the names differ")
            }
            Msg::FixedSize { name } => format!("fixed '{name}' has a different size"),
            Msg::Decimal => "the decimal precision or scale differs".to_string(),
            Msg::EnumSymbol { name, symbol } => format!(
                "enum '{name}': symbol '{symbol}' of the {writer} schema is unknown to the \
                 {reader} schema, which has no default symbol"
            ),
            Msg::MissingDefault { record, field } => format!(
                "the {reader} schema requires field '{field}' of record '{record}', which has no \
                 default and is missing from the {writer} schema"
            ),
            Msg::UnionWriter { branch } => format!(
                "the {writer} schema may write {branch}, which the {reader} schema cannot read"
            ),
            Msg::UnionReader { branch } => format!(
                "no branch of the {reader} union can read {branch} written by the {writer} schema"
            ),
        };
        format!("{}: {text}", shorten_path(&path))
    }
}

enum Stop {
    No(Why),
    Complex,
}

type Verdict = Result<(), Stop>;

fn label(node: &Node) -> String {
    match &node.kind {
        Kind::Null => "null".to_string(),
        Kind::Boolean => "boolean".to_string(),
        Kind::Int => "int".to_string(),
        Kind::Long => "long".to_string(),
        Kind::Float => "float".to_string(),
        Kind::Double => "double".to_string(),
        Kind::Bytes { .. } => "bytes".to_string(),
        Kind::String => "string".to_string(),
        Kind::Fixed { name, .. } => format!("fixed '{name}'"),
        Kind::Enum { name, .. } => format!("enum '{name}'"),
        Kind::Array(_) => "an array".to_string(),
        Kind::Map(_) => "a map".to_string(),
        Kind::Union(_) => "a union".to_string(),
        Kind::Record { name, .. } => format!("record '{name}'"),
    }
}

/// The unqualified name of a named type, with the aliases a reader accepts.
fn named(kind: &Kind) -> Option<(&str, &[String])> {
    match kind {
        Kind::Fixed { name, aliases, .. }
        | Kind::Enum { name, aliases, .. }
        | Kind::Record { name, aliases, .. } => Some((name, aliases)),
        _ => None,
    }
}

fn names_match(writer: &Kind, reader: &Kind) -> bool {
    match (named(writer), named(reader)) {
        (Some((w, _)), Some((r, reader_aliases))) => {
            w == r || reader_aliases.iter().any(|a| a == w)
        }
        _ => true,
    }
}

/// Primitive promotions: int -> long, float, double; long -> float, double;
/// float -> double; string <-> bytes.
fn promotes(writer: &Kind, reader: &Kind) -> bool {
    matches!(
        (writer, reader),
        (Kind::Null, Kind::Null)
            | (Kind::Boolean, Kind::Boolean)
            | (
                Kind::Int,
                Kind::Int | Kind::Long | Kind::Float | Kind::Double
            )
            | (Kind::Long, Kind::Long | Kind::Float | Kind::Double)
            | (Kind::Float, Kind::Float | Kind::Double)
            | (Kind::Double, Kind::Double)
            | (
                Kind::String | Kind::Bytes { .. },
                Kind::String | Kind::Bytes { .. }
            )
    )
}

struct Compat<'a> {
    writer: &'a [Node],
    reader: &'a [Node],
    budget: Budget,
    memo: HashMap<(NodeId, NodeId), Option<Why>>,
    /// Pairs being decided, with their position on the stack: meeting one
    /// again assumes it holds (the greatest fixpoint, as recursive types need).
    active: HashMap<(NodeId, NodeId), usize>,
    /// The shallowest active pair the current subtree leaned on.
    lowest_assumption: usize,
}

impl Compat<'_> {
    fn charge(&mut self, units: u64) -> Result<(), Stop> {
        self.budget.charge(units).map_err(|_| Stop::Complex)
    }

    fn reads(&mut self, w: NodeId, r: NodeId) -> Verdict {
        self.charge(1)?;
        let key = (w, r);
        if let Some(known) = self.memo.get(&key) {
            return known.clone().map_or(Ok(()), |why| Err(Stop::No(why)));
        }
        if let Some(&at) = self.active.get(&key) {
            self.lowest_assumption = self.lowest_assumption.min(at);
            return Ok(());
        }
        if self.active.len() >= MAX_COMPAT_DEPTH {
            return Err(Stop::Complex);
        }
        let at = self.active.len();
        self.active.insert(key, at);
        let outer = std::mem::replace(&mut self.lowest_assumption, usize::MAX);
        let verdict = self.decide(w, r);
        self.active.remove(&key);
        let leaned = self.lowest_assumption;
        self.lowest_assumption = outer.min(if leaned < at { leaned } else { usize::MAX });
        // A refusal is final even when reached under an assumption (assuming
        // more can only make a pair easier); an acceptance that leaned on a
        // pair still being decided is not yet proven.
        if self.memo.len() < MAX_COMPAT_MEMO {
            match &verdict {
                Err(Stop::No(why)) => {
                    self.memo.insert(key, Some(why.clone()));
                }
                Ok(()) if leaned >= at => {
                    self.memo.insert(key, None);
                }
                _ => {}
            }
        }
        verdict
    }

    fn any_reader_branch(&mut self, w: NodeId, branches: &[NodeId]) -> Verdict {
        for &r in branches {
            match self.reads(w, r) {
                Ok(()) => return Ok(()),
                Err(Stop::No(_)) => {}
                Err(Stop::Complex) => return Err(Stop::Complex),
            }
        }
        Err(Stop::No(Why::new(Msg::UnionReader {
            branch: label(&self.writer[w]),
        })))
    }

    fn decide(&mut self, w: NodeId, r: NodeId) -> Verdict {
        // Slices of the node tables outlive the `&mut self` borrows below.
        let (writer_nodes, reader_nodes) = (self.writer, self.reader);
        let (wk, rk) = (&writer_nodes[w].kind, &reader_nodes[r].kind);
        match (wk, rk) {
            (Kind::Union(wb), Kind::Union(rb)) => {
                self.charge((wb.len() * rb.len()) as u64)?;
                for &branch in wb {
                    match self.any_reader_branch(branch, rb) {
                        Ok(()) => {}
                        Err(Stop::No(_)) => {
                            return Err(Stop::No(Why::new(Msg::UnionWriter {
                                branch: label(&writer_nodes[branch]),
                            })))
                        }
                        Err(Stop::Complex) => return Err(Stop::Complex),
                    }
                }
                Ok(())
            }
            (Kind::Union(wb), _) => {
                for &branch in wb {
                    match self.reads(branch, r) {
                        Ok(()) => {}
                        Err(Stop::No(_)) => {
                            return Err(Stop::No(Why::new(Msg::UnionWriter {
                                branch: label(&writer_nodes[branch]),
                            })))
                        }
                        Err(Stop::Complex) => return Err(Stop::Complex),
                    }
                }
                Ok(())
            }
            (_, Kind::Union(rb)) => {
                self.charge(rb.len() as u64)?;
                self.any_reader_branch(w, rb)
            }
            (Kind::Bytes { decimal: wd }, Kind::Bytes { decimal: rd })
            | (Kind::Fixed { decimal: wd, .. }, Kind::Fixed { decimal: rd, .. })
                if matches!((wd, rd), (Some(a), Some(b)) if a != b) =>
            {
                Err(Stop::No(Why::new(Msg::Decimal)))
            }
            (Kind::Fixed { name, size: ws, .. }, Kind::Fixed { size: rs, .. }) => {
                if !names_match(wk, rk) {
                    return Err(Stop::No(Why::new(Msg::Names {
                        writer: label(&writer_nodes[w]),
                        reader: label(&reader_nodes[r]),
                    })));
                }
                if ws != rs {
                    return Err(Stop::No(Why::new(Msg::FixedSize { name: name.clone() })));
                }
                Ok(())
            }
            (
                Kind::Enum {
                    name, symbols: ws, ..
                },
                Kind::Enum {
                    symbols: rs,
                    has_default,
                    ..
                },
            ) => {
                if !names_match(wk, rk) {
                    return Err(Stop::No(Why::new(Msg::Names {
                        writer: label(&writer_nodes[w]),
                        reader: label(&reader_nodes[r]),
                    })));
                }
                if *has_default {
                    return Ok(());
                }
                self.charge((ws.len() + rs.len()) as u64)?;
                let known: HashSet<&String> = rs.iter().collect();
                match ws.iter().find(|s| !known.contains(s)) {
                    None => Ok(()),
                    Some(symbol) => Err(Stop::No(Why::new(Msg::EnumSymbol {
                        name: name.clone(),
                        symbol: symbol.clone(),
                    }))),
                }
            }
            (Kind::Array(wi), Kind::Array(ri)) => self
                .reads(*wi, *ri)
                .map_err(|stop| within(stop, Step::Item)),
            (Kind::Map(wv), Kind::Map(rv)) => self
                .reads(*wv, *rv)
                .map_err(|stop| within(stop, Step::Entry)),
            (
                Kind::Record {
                    fields: wf,
                    name: record,
                    ..
                },
                Kind::Record { fields: rf, .. },
            ) => {
                if !names_match(wk, rk) {
                    return Err(Stop::No(Why::new(Msg::Names {
                        writer: label(&writer_nodes[w]),
                        reader: label(&reader_nodes[r]),
                    })));
                }
                self.charge((wf.len() + rf.len()) as u64)?;
                let by_name: HashMap<&str, &Field> =
                    wf.iter().map(|f| (f.name.as_str(), f)).collect();
                for field in rf {
                    // The reader's own name first, then its aliases; a writer
                    // alias never matches.
                    let source = std::iter::once(field.name.as_str())
                        .chain(field.aliases.iter().map(String::as_str))
                        .find_map(|n| by_name.get(n));
                    match source {
                        Some(from) => self
                            .reads(from.node, field.node)
                            .map_err(|stop| within(stop, Step::Field(field.name.clone())))?,
                        None if field.has_default => {}
                        None => {
                            return Err(Stop::No(
                                Why::new(Msg::MissingDefault {
                                    record: record.clone(),
                                    field: field.name.clone(),
                                })
                                .within(Step::Field(field.name.clone())),
                            ))
                        }
                    }
                }
                Ok(())
            }
            _ if promotes(wk, rk) => Ok(()),
            _ => Err(Stop::No(Why::new(Msg::Type {
                writer: label(&writer_nodes[w]),
                reader: label(&reader_nodes[r]),
            }))),
        }
    }
}

fn within(stop: Stop, step: Step) -> Stop {
    match stop {
        Stop::No(why) => Stop::No(why.within(step)),
        Stop::Complex => Stop::Complex,
    }
}

/// Whether data written under `writer` can all be read under `reader`.
fn reads(writer: &Compiled, reader: &Compiled) -> Verdict {
    Compat {
        writer: &writer.nodes,
        reader: &reader.nodes,
        budget: Budget::new(MAX_COMPAT_WORK),
        memo: HashMap::new(),
        active: HashMap::new(),
        lowest_assumption: usize::MAX,
    }
    .reads(writer.root, reader.root)
}

// =============================================================================
// Projection
// =============================================================================

fn collect_definitions<'s>(schema: &'s Schema, out: &mut HashMap<String, &'s Schema>) {
    match schema {
        Schema::Record(r) => {
            out.entry(r.name.fullname(None)).or_insert(schema);
            for f in &r.fields {
                collect_definitions(&f.schema, out);
            }
        }
        Schema::Enum(e) => {
            out.entry(e.name.fullname(None)).or_insert(schema);
        }
        Schema::Fixed(f) | Schema::Uuid(UuidSchema::Fixed(f)) | Schema::Duration(f) => {
            out.entry(f.name.fullname(None)).or_insert(schema);
        }
        Schema::Decimal(d) => {
            if let InnerDecimalSchema::Fixed(f) = &d.inner {
                out.entry(f.name.fullname(None)).or_insert(schema);
            }
        }
        Schema::Array(a) => collect_definitions(&a.items, out),
        Schema::Map(m) => collect_definitions(&m.types, out),
        Schema::Union(u) => u
            .variants()
            .iter()
            .for_each(|b| collect_definitions(b, out)),
        _ => {}
    }
}

/// `schema` as it stands in the projection: a named type is written in full
/// at its first mention in the output and by reference afterwards, even when
/// its original definition was in a field the projection dropped.
fn rebuild(
    schema: &Schema,
    definitions: &HashMap<String, &Schema>,
    written: &mut HashSet<String>,
) -> Result<Schema, SchemaError> {
    Ok(match schema {
        Schema::Ref { name } => {
            let full = name.fullname(None);
            if written.contains(&full) {
                schema.clone()
            } else if let Some(definition) = definitions.get(&full) {
                rebuild(definition, definitions, written)?
            } else {
                schema.clone()
            }
        }
        Schema::Record(r) => {
            written.insert(r.name.fullname(None));
            let fields = r
                .fields
                .iter()
                .map(|f| rebuild_field(f, definitions, written))
                .collect::<Result<Vec<_>, _>>()?;
            record_with(r, fields)
        }
        Schema::Enum(e) => {
            written.insert(e.name.fullname(None));
            schema.clone()
        }
        Schema::Fixed(f) | Schema::Uuid(UuidSchema::Fixed(f)) | Schema::Duration(f) => {
            written.insert(f.name.fullname(None));
            schema.clone()
        }
        Schema::Decimal(d) => {
            if let InnerDecimalSchema::Fixed(f) = &d.inner {
                written.insert(f.name.fullname(None));
            }
            schema.clone()
        }
        Schema::Array(a) => Schema::Array(ArraySchema {
            items: Box::new(rebuild(&a.items, definitions, written)?),
            attributes: a.attributes.clone(),
        }),
        Schema::Map(m) => Schema::Map(MapSchema {
            types: Box::new(rebuild(&m.types, definitions, written)?),
            attributes: m.attributes.clone(),
        }),
        Schema::Union(u) => {
            let branches = u
                .variants()
                .iter()
                .map(|b| rebuild(b, definitions, written))
                .collect::<Result<Vec<_>, _>>()?;
            Schema::Union(
                UnionSchema::new(branches)
                    .map_err(|e| invalid(format!("not a valid Avro schema: {e}")))?,
            )
        }
        other => other.clone(),
    })
}

fn rebuild_field(
    field: &RecordField,
    definitions: &HashMap<String, &Schema>,
    written: &mut HashSet<String>,
) -> Result<RecordField, SchemaError> {
    Ok(RecordField {
        schema: rebuild(&field.schema, definitions, written)?,
        ..field.clone()
    })
}

fn record_with(record: &RecordSchema, fields: Vec<RecordField>) -> Schema {
    let mut lookup = BTreeMap::new();
    for (position, field) in fields.iter().enumerate() {
        lookup.insert(field.name.clone(), position);
        for alias in &field.aliases {
            lookup.insert(alias.clone(), position);
        }
    }
    Schema::Record(RecordSchema {
        name: record.name.clone(),
        aliases: record.aliases.clone(),
        doc: record.doc.clone(),
        fields,
        lookup,
        attributes: record.attributes.clone(),
    })
}

// =============================================================================
// SchemaKindOps
// =============================================================================

pub struct AvroOps;

impl SchemaKindOps for AvroOps {
    fn compile(&self, schema_text: &str) -> Result<CompiledSchema, SchemaError> {
        parse_text(schema_text).map(|(_, compiled)| CompiledSchema::Avro(compiled))
    }

    fn validate_metered(
        &self,
        compiled: &CompiledSchema,
        payload: &[u8],
        shared: &mut ValidationBudget,
    ) -> Result<(), SchemaError> {
        let CompiledSchema::Avro(c) = compiled else {
            return Err(invalid("validate called with a non-avro compiled schema"));
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
        let (schema, _) = parse_text(schema_text)?;
        let Schema::Record(root) = &schema else {
            return Err(invalid(
                "root schema must be a record to derive a projection",
            ));
        };
        let mut definitions = HashMap::new();
        collect_definitions(&schema, &mut definitions);
        let mut written = HashSet::new();
        written.insert(root.name.fullname(None));
        let fields = root
            .fields
            .iter()
            .filter(|f| allowed.contains(&f.name))
            .map(|f| rebuild_field(f, &definitions, &mut written))
            .collect::<Result<Vec<_>, _>>()?;
        let derived = serde_json::to_string(&record_with(root, fields))
            .map_err(|e| invalid(format!("the projection cannot be written: {e}")))?;
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
        let (_, old) = parse_text(old_schema_text)?;
        let (_, new) = parse_text(new_schema_text)?;
        let check =
            |writer: &Compiled, reader: &Compiled, lead: &str, labels: (&str, &str)| match reads(
                writer, reader,
            ) {
                Ok(()) => Ok(()),
                Err(Stop::No(why)) => Err(SchemaError::Incompatible(format!(
                    "{lead} - {}",
                    why.render(labels.0, labels.1)
                ))),
                Err(Stop::Complex) => Err(SchemaError::LimitExceeded(TOO_COMPLEX.to_string())),
            };
        let backward = || {
            check(
                &old,
                &new,
                "backward: data written under the old schema may not be readable with the new one",
                ("old", "new"),
            )
        };
        let forward = || {
            check(
                &new,
                &old,
                "forward: data written under the new schema may not be readable with the old one",
                ("new", "old"),
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

pub(super) static AVRO_OPS: AvroOps = AvroOps;

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use apache_avro::reader::datum::GenericDatumReader;
    use apache_avro::schema_compatibility::{Compatibility as LibraryVerdict, SchemaCompatibility};

    use super::*;
    use crate::bus::schema_registry::{fixtures, SchemaType};

    fn zz(n: i64) -> Vec<u8> {
        let mut v = ((n << 1) ^ (n >> 63)) as u64;
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

    fn text(s: &str) -> Vec<u8> {
        let mut out = zz(s.len() as i64);
        out.extend_from_slice(s.as_bytes());
        out
    }

    fn rec(fields: &str) -> String {
        format!(r#"{{"type":"record","name":"R","fields":[{fields}]}}"#)
    }

    fn compiled(schema: &str) -> CompiledSchema {
        AVRO_OPS
            .compile(schema)
            .unwrap_or_else(|e| panic!("{schema}: {e}"))
    }

    fn check(schema: &str, payload: &[u8]) -> Result<(), SchemaError> {
        AVRO_OPS.validate(&compiled(schema), payload)
    }

    fn refusal(schema: &str) -> String {
        match AVRO_OPS.compile(schema) {
            Err(SchemaError::Invalid(m)) => m,
            other => panic!("expected a refusal of {schema:.80}, got {other:?}"),
        }
    }

    fn violation(result: Result<(), SchemaError>) -> String {
        match result {
            Err(SchemaError::Violation(m)) => m,
            other => panic!("expected a violation, got {other:?}"),
        }
    }

    fn limit(result: Result<(), SchemaError>) -> String {
        match result {
            Err(SchemaError::LimitExceeded(m)) => m,
            other => panic!("expected LimitExceeded, got {other:?}"),
        }
    }

    /// How many bytes the library's own decoder takes from `payload`; `None`
    /// when it refuses. It runs on a big stack: in a debug build the library's
    /// recursive decoder needs tens of kilobytes per nesting level.
    fn library_consumes(schema: &str, payload: &[u8]) -> Option<usize> {
        let (schema, payload) = (schema.to_string(), payload.to_vec());
        std::thread::Builder::new()
            .stack_size(256 * 1024 * 1024)
            .spawn(move || {
                let schema = Schema::parse_str(&schema).unwrap();
                let mut rest = payload.as_slice();
                GenericDatumReader::builder(&schema)
                    .build()
                    .ok()?
                    .read_value(&mut rest)
                    .ok()?;
                Some(payload.len() - rest.len())
            })
            .unwrap()
            .join()
            .unwrap()
    }

    // -------------------------------------------------------------------------
    // Golden fixtures
    // -------------------------------------------------------------------------

    #[test]
    fn golden_fixtures_validate_as_named() {
        let schema = fixtures::read("avro", "order.avsc");
        let c = compiled(&schema);
        let cases = fixtures::cases("avro", "datum");
        assert!(cases.len() >= 18);
        for case in cases {
            let verdict = AVRO_OPS.validate(&c, &case.payload);
            match &case.expect {
                None => assert!(verdict.is_ok(), "{}: {verdict:?}", case.name),
                Some(expected) => assert!(
                    matches!(&verdict, Err(SchemaError::Violation(m)) if m.contains(expected.as_str())),
                    "{}: expected {expected:?}, got {verdict:?}",
                    case.name
                ),
            }
        }
    }

    /// The hand-written walker may be stricter than the library but must
    /// never accept what the library cannot decode, and must consume the
    /// same number of bytes when both accept.
    #[test]
    fn valid_fixtures_decode_with_the_library_and_consume_every_byte() {
        let schema = fixtures::read("avro", "order.avsc");
        for case in fixtures::cases("avro", "datum")
            .into_iter()
            .filter(|c| c.expect.is_none())
        {
            assert_eq!(
                library_consumes(&schema, &case.payload),
                Some(case.payload.len()),
                "{}",
                case.name
            );
        }
    }

    #[test]
    fn the_walker_never_accepts_what_the_library_rejects() {
        let order = fixtures::read("avro", "order.avsc");
        let list = r#"{"type":"record","name":"L","fields":[{"name":"v","type":"int"},{"name":"next","type":["null","L"]}]}"#;
        let mixed = rec(
            r#"{"name":"a","type":{"type":"array","items":["null","long",{"type":"map","values":"bytes"}]}},
               {"name":"b","type":{"type":"fixed","name":"F","size":3}},
               {"name":"c","type":"float"},{"name":"d","type":"string"}"#,
        );
        let mut seeds: Vec<(String, Vec<u8>)> = fixtures::cases("avro", "datum")
            .into_iter()
            .filter(|c| c.expect.is_none())
            .map(|c| (order.clone(), c.payload))
            .collect();
        let mut chain = Vec::new();
        for v in [3, 5, 8] {
            chain.extend(zz(v));
            chain.extend(zz(1));
        }
        chain.extend(zz(13));
        chain.extend(zz(0));
        seeds.push((list.to_string(), chain));
        // a: [null, long 5, {"x": "abc"}]; b: fixed "xyz"; c: float; d: "done".
        let mut m = zz(3);
        m.extend(zz(0));
        m.extend(zz(1));
        m.extend(zz(5));
        m.extend(zz(2));
        m.extend(zz(1));
        m.extend(text("x"));
        m.extend(zz(3));
        m.extend(b"abc");
        m.extend(zz(0));
        m.extend(zz(0));
        m.extend(b"xyz");
        m.extend([0, 0, 128, 63]);
        m.extend(text("done"));
        seeds.push((mixed, m));

        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let (mut accepted, mut total) = (0, 0);
        for (schema, seed) in &seeds {
            assert!(check(schema, seed).is_ok(), "seed must be valid");
            let c = compiled(schema);
            for _ in 0..600 {
                let mut bytes = seed.clone();
                match next() % 4 {
                    0 => {
                        let at = (next() as usize) % bytes.len();
                        bytes[at] = next() as u8;
                    }
                    1 => bytes.truncate((next() as usize) % bytes.len()),
                    2 => {
                        let at = (next() as usize) % bytes.len();
                        bytes.insert(at, next() as u8);
                    }
                    _ => {
                        let at = (next() as usize) % bytes.len();
                        bytes.remove(at);
                    }
                }
                total += 1;
                if AVRO_OPS.validate(&c, &bytes).is_ok() {
                    accepted += 1;
                    assert_eq!(
                        library_consumes(schema, &bytes),
                        Some(bytes.len()),
                        "accepted {bytes:?} that the library does not take whole"
                    );
                }
            }
        }
        assert!(accepted > 0 && accepted < total, "{accepted}/{total}");
    }

    #[test]
    fn the_library_accepts_truncated_booleans_and_unions_and_the_walker_does_not() {
        let boolean = rec(r#"{"name":"b","type":"boolean"}"#);
        assert_eq!(
            library_consumes(&boolean, b""),
            Some(0),
            "library quirk this module guards against"
        );
        assert!(violation(check(&boolean, b"")).contains("/b: payload ends"));
        let union = rec(r#"{"name":"u","type":["null","int"]}"#);
        assert_eq!(library_consumes(&union, b""), Some(0));
        assert!(violation(check(&union, b"")).contains("/u: payload ends"));
    }

    // -------------------------------------------------------------------------
    // Wire format
    // -------------------------------------------------------------------------

    #[test]
    fn every_primitive_and_the_framing_formats() {
        let all = rec(
            r#"{"name":"n","type":"null"},{"name":"b","type":"boolean"},{"name":"i","type":"int"},
               {"name":"l","type":"long"},{"name":"f","type":"float"},{"name":"d","type":"double"},
               {"name":"y","type":"bytes"},{"name":"s","type":"string"}"#,
        );
        let mut datum = vec![1];
        datum.extend(zz(-7));
        datum.extend(zz(i64::MIN));
        datum.extend([0; 4]);
        datum.extend([0; 8]);
        datum.extend(zz(2));
        datum.extend([9, 9]);
        datum.extend(text("żółć"));
        assert!(check(&all, &datum).is_ok());
        assert_eq!(library_consumes(&all, &datum), Some(datum.len()));

        // A container file, a single-object header and a Confluent header are
        // not datums: each is refused rather than guessed at.
        for header in [
            &b"Obj\x01"[..],
            &[0xC3, 0x01, 1, 2, 3, 4, 5, 6, 7, 8][..],
            &[0, 0, 0, 0, 7][..],
        ] {
            let mut framed = header.to_vec();
            framed.extend(&datum);
            assert!(
                matches!(check(&all, &framed), Err(SchemaError::Violation(_))),
                "{header:?}"
            );
        }
    }

    #[test]
    fn the_root_may_be_any_type_and_null_is_an_empty_datum() {
        assert!(check(r#""null""#, b"").is_ok());
        assert!(violation(check(r#""null""#, b"\x00")).contains("<root>: bytes remain"));
        assert!(check(r#""string""#, &text("abc")).is_ok());
        assert!(check(r#"["null","long"]"#, &[2, 14]).is_ok());
    }

    #[test]
    fn trailing_truncated_and_ill_formed_values_are_violations() {
        let s = rec(r#"{"name":"a","type":"string"}"#);
        assert!(check(&s, &text("ok")).is_ok());
        let mut trailing = text("ok");
        trailing.push(0);
        assert!(violation(check(&s, &trailing)).contains("bytes remain after the value"));
        assert!(violation(check(&s, b"")).contains("/a: payload ends"));
        assert!(violation(check(&s, &text("ok")[..2])).contains("/a: declared length exceeds"));
        assert!(violation(check(&s, &[4, 0xff, 0xfe])).contains("/a: string is not valid UTF-8"));
        assert!(violation(check(&s, &[3])).contains("/a: length is negative"));
        // An overlong varint, with and without a continuation on the tenth byte.
        assert!(violation(check(&s, &[0x80; 10])).contains("integer encoding"));
        assert!(violation(check(&s, &[0xff; 11])).contains("integer encoding"));
    }

    #[test]
    fn array_and_map_blocks_obey_their_declared_sizes() {
        let arr = rec(r#"{"name":"a","type":{"type":"array","items":"int"}}"#);
        // Two blocks: [1] then [2,3].
        let two_blocks = [&[2, 2][..], &[4, 4, 6][..], &[0][..]].concat();
        assert!(check(&arr, &two_blocks).is_ok());
        // Negative count with a correct size (items 1,2 take two bytes).
        let sized = [&zz(-2)[..], &zz(2)[..], &[2, 4][..], &[0][..]].concat();
        assert!(check(&arr, &sized).is_ok());
        let wrong = [&zz(-2)[..], &zz(3)[..], &[2, 4][..], &[0][..]].concat();
        assert!(violation(check(&arr, &wrong)).contains("/a/[]: block byte size"));
        // A declared size beyond the payload is refused before any item.
        let beyond = [&zz(-2)[..], &zz(99)[..]].concat();
        assert!(violation(check(&arr, &beyond)).contains("/a/[]: block byte size"));
        let map = rec(r#"{"name":"m","type":{"type":"map","values":"int"}}"#);
        let entries = [&[2][..], &text("k")[..], &[2][..], &[0][..]].concat();
        assert!(check(&map, &entries).is_ok());
        let bad_key = [&[2][..], &[2, 0xff][..], &[2][..], &[0][..]].concat();
        assert!(violation(check(&map, &bad_key)).contains("/m/{}: string is not valid UTF-8"));
    }

    #[test]
    fn logical_types_are_checked_at_their_underlying_type() {
        let s = rec(r#"{"name":"d","type":{"type":"int","logicalType":"date"}},
               {"name":"t","type":{"type":"long","logicalType":"timestamp-millis"}},
               {"name":"m","type":{"type":"bytes","logicalType":"decimal","precision":6,"scale":2}},
               {"name":"u","type":{"type":"string","logicalType":"uuid"}},
               {"name":"x","type":{"type":"fixed","name":"Dec","size":4,"logicalType":"decimal","precision":8,"scale":2}}"#);
        let mut datum = zz(19000);
        datum.extend(zz(1_700_000_000_000));
        datum.extend(zz(2));
        datum.extend([1, 2]);
        datum.extend(text("123e4567-e89b-12d3-a456-426614174000"));
        datum.extend([0, 0, 1, 0]);
        assert!(check(&s, &datum).is_ok());
        assert_eq!(library_consumes(&s, &datum), Some(datum.len()));
        // The walker does not parse a UUID; it checks the string.
        let mut odd = zz(19000);
        odd.extend(zz(1_700_000_000_000));
        odd.extend(zz(2));
        odd.extend([1, 2]);
        odd.extend(text("not-really-a-uuid"));
        odd.extend([0, 0, 1, 0]);
        assert!(check(&s, &odd).is_ok());
    }

    #[test]
    fn a_name_defined_twice_is_refused() {
        let twice = rec(
            r#"{"name":"a","type":{"type":"enum","name":"E","symbols":["X"]}},
               {"name":"b","type":{"type":"enum","name":"E","symbols":["Y","Z"]}}"#,
        );
        assert!(refusal(&twice).contains("type 'E' is defined more than once"));
        // The same unqualified name in two namespaces is two types.
        let spaced = rec(
            r#"{"name":"a","type":{"type":"enum","name":"E","namespace":"p","symbols":["X"]}},
               {"name":"b","type":{"type":"enum","name":"E","namespace":"q","symbols":["Y"]}}"#,
        );
        assert!(AVRO_OPS.compile(&spaced).is_ok());
    }

    #[test]
    fn named_types_resolve_by_namespace_and_alias() {
        let s = r#"{"type":"record","name":"Outer","namespace":"a.b","fields":[
            {"name":"x","type":{"type":"enum","name":"E","aliases":["Old"],"symbols":["P","Q"]}},
            {"name":"y","type":"E"},
            {"name":"z","type":"a.b.E"},
            {"name":"w","type":"Old"}]}"#;
        assert!(check(s, &[0, 2, 0, 2]).is_ok());
        assert!(violation(check(s, &[0, 4, 0, 0])).contains("/y: enum index"));
    }

    // -------------------------------------------------------------------------
    // Recursion and productivity
    // -------------------------------------------------------------------------

    const LIST: &str = r#"{"type":"record","name":"L","fields":[{"name":"v","type":"int"},{"name":"next","type":["null","L"]}]}"#;

    fn list_datum(length: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..length {
            out.extend(zz(1));
            out.extend(zz(1));
        }
        out.extend(zz(1));
        out.extend(zz(0));
        out
    }

    #[test]
    fn a_recursive_schema_validates_and_deep_data_is_refused_not_overflowed() {
        assert!(check(LIST, &list_datum(20)).is_ok());
        assert_eq!(
            library_consumes(LIST, &list_datum(20)),
            Some(list_datum(20).len())
        );
        // 100 000 levels cost 200 000 bytes of payload; the walk stops at the
        // depth cap instead of recursing that far.
        let message = limit(check(LIST, &list_datum(100_000)));
        assert!(message.contains("nested too deeply"), "{message}");
        assert!(message.starts_with('/'), "{message}");
    }

    #[test]
    fn a_deep_datum_needs_little_stack() {
        // A 512 KiB stack, a quarter of a default thread's, in a debug build.
        let outcome = std::thread::Builder::new()
            .stack_size(512 * 1024)
            .spawn(|| {
                let at_limit = check(LIST, &list_datum(MAX_VALUE_DEPTH - 1));
                let beyond = check(LIST, &list_datum(MAX_VALUE_DEPTH + 1));
                (
                    at_limit.is_ok(),
                    matches!(beyond, Err(SchemaError::LimitExceeded(_))),
                )
            })
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(outcome, (true, true));
    }

    #[test]
    fn a_type_with_no_finite_value_is_refused() {
        for schema in [
            r#"{"type":"record","name":"A","fields":[{"name":"a","type":"A"}]}"#,
            r#"{"type":"record","name":"A","fields":[{"name":"b","type":{"type":"record","name":"B","fields":[{"name":"a","type":"A"}]}}]}"#,
        ] {
            assert!(
                refusal(schema).contains("can never produce a value"),
                "{schema}"
            );
        }
        // An optional branch, an array or a map is an escape.
        for field in [
            r#"["null","A"]"#,
            r#"{"type":"array","items":"A"}"#,
            r#"{"type":"map","values":"A"}"#,
        ] {
            let schema = format!(
                r#"{{"type":"record","name":"A","fields":[{{"name":"a","type":{field}}}]}}"#
            );
            assert!(AVRO_OPS.compile(&schema).is_ok(), "{schema}");
        }
    }

    // -------------------------------------------------------------------------
    // Compile refusals
    // -------------------------------------------------------------------------

    #[test]
    fn compile_refuses_what_it_cannot_check() {
        assert!(refusal("").contains("schema text is empty"));
        assert!(refusal("   ").contains("schema text is empty"));
        assert!(refusal("not json").contains("the schema is not valid JSON"));
        assert!(refusal(r#"{"type":"nonsense"}"#).contains("not a valid Avro schema"));
        assert!(refusal(r#"{"type":"record","name":"R","fields":[1,2]}"#)
            .contains("a record lists a field that is not an object"));
        assert!(refusal(r#"{"type":"record","name":"R","fields":{}}"#).contains("not an array"));
        assert!(refusal(&rec(
            r#"{"name":"a","type":"int"},{"name":"a","type":"long"}"#
        ))
        .contains("not a valid Avro schema"));
        assert!(
            refusal(&rec(r#"{"name":"a","type":"Missing"}"#)).contains("not a valid Avro schema")
        );
        let big = " ".repeat(MAX_SCHEMA_TEXT_BYTES + 1);
        assert!(refusal(&big).contains("exceeding the 262144-byte limit"));
    }

    #[test]
    fn compile_caps_the_size_of_the_schema() {
        let fields = |n: usize| {
            (0..n)
                .map(|i| format!(r#"{{"name":"f{i}","type":"int"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        };
        assert!(AVRO_OPS.compile(&rec(&fields(MAX_RECORD_FIELDS))).is_ok());
        assert!(refusal(&rec(&fields(MAX_RECORD_FIELDS + 1)))
            .contains("record 'R' has more than 1024 fields"));

        let enums = |n: usize| {
            (0..n)
                .map(|i| format!(r#"{{"type":"enum","name":"E{i}","symbols":["A"]}}"#))
                .collect::<Vec<_>>()
                .join(",")
        };
        assert!(AVRO_OPS
            .compile(&rec(&format!(
                r#"{{"name":"u","type":[{}]}}"#,
                enums(MAX_UNION_BRANCHES)
            )))
            .is_ok());
        assert!(refusal(&rec(&format!(
            r#"{{"name":"u","type":[{}]}}"#,
            enums(MAX_UNION_BRANCHES + 1)
        )))
        .contains("a union has more than 64 branches"));

        let symbols = |n: usize| {
            (0..n)
                .map(|i| format!("\"S{i}\""))
                .collect::<Vec<_>>()
                .join(",")
        };
        assert!(refusal(&format!(
            r#"{{"type":"enum","name":"E","symbols":[{}]}}"#,
            symbols(MAX_ENUM_SYMBOLS + 1)
        ))
        .contains("enum 'E' has more than 1024 symbols"));
        assert!(
            refusal(r#"{"type":"fixed","name":"F","size":999999999999}"#)
                .contains("fixed 'F' is larger than 16777216 bytes")
        );

        // Five records of 1024 int fields each: over the type cap although each is under the field cap.
        let nested = (0..5)
            .map(|k| {
                format!(
                    r#"{{"name":"g{k}","type":{{"type":"record","name":"G{k}","fields":[{}]}}}}"#,
                    fields(MAX_RECORD_FIELDS)
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        assert!(refusal(&rec(&nested)).contains("the schema has more than 4096 types"));
    }

    #[test]
    fn a_default_that_does_not_fit_its_type_is_refused() {
        for field in [
            r#"{"name":"a","type":"int","default":"x"}"#,
            r#"{"name":"a","type":"string","default":1}"#,
            r#"{"name":"a","type":["null","string"],"default":"x"}"#,
            r#"{"name":"a","type":{"type":"enum","name":"E","symbols":["A"]},"default":"B"}"#,
            r#"{"name":"a","type":{"type":"array","items":"int"},"default":["x"]}"#,
        ] {
            let reason = refusal(&rec(field));
            assert!(
                reason.contains("not a valid Avro schema")
                    || reason.contains("does not fit the first branch of its union"),
                "{field}: {reason}"
            );
        }
        for field in [
            r#"{"name":"a","type":"int","default":3}"#,
            r#"{"name":"a","type":["null","string"],"default":null}"#,
            r#"{"name":"a","type":["string","null"],"default":"x"}"#,
            r#"{"name":"a","type":{"type":"array","items":"int"},"default":[1]}"#,
        ] {
            assert!(AVRO_OPS.compile(&rec(field)).is_ok(), "{field}");
        }
    }

    #[test]
    fn compile_caps_nesting_whatever_the_json_parser_allows() {
        let nest = |depth: usize| {
            let mut s = String::from("\"int\"");
            for _ in 0..depth {
                s = format!(r#"{{"type":"array","items":{s}}}"#);
            }
            s
        };
        assert!(AVRO_OPS.compile(&nest(MAX_SCHEMA_DEPTH)).is_ok());
        assert!(refusal(&nest(MAX_SCHEMA_DEPTH + 1)).contains("nested deeper than 64 levels"));
        // Beyond the JSON parser's own limit, and absurd depth: refused, no stack overflow.
        assert!(refusal(&nest(200)).contains("not valid JSON"));
        let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
        assert!(refusal(&deep).contains("not valid JSON"));
    }

    #[test]
    fn compile_does_not_panic_on_mangled_schemas() {
        let seeds = [
            fixtures::read("avro", "order.avsc"),
            LIST.to_string(),
            r#"{"type":"record","name":"A","namespace":"x.y","aliases":["B"],"fields":[{"name":"e","type":{"type":"enum","name":"E","symbols":["P"],"default":"P"}},{"name":"f","type":{"type":"fixed","name":"F","size":2}},{"name":"g","type":["null",{"type":"map","values":"A"}],"default":null}]}"#.to_string(),
        ];
        let pieces = [
            "\"",
            "{",
            "}",
            "[",
            "]",
            ",",
            ":",
            "null",
            "\"type\"",
            "\"name\"",
            "0",
            "-1",
            "\"fields\"",
            "\"symbols\"",
            "\"size\"",
        ];
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for seed in &seeds {
            for _ in 0..1500 {
                let mut bytes = seed.clone().into_bytes();
                for _ in 0..1 + next() % 3 {
                    let at = (next() as usize) % bytes.len();
                    match next() % 3 {
                        0 => {
                            bytes.remove(at);
                        }
                        1 => {
                            let piece = pieces[(next() as usize) % pieces.len()];
                            bytes.splice(at..at, piece.bytes());
                        }
                        _ => bytes[at] = pieces[(next() as usize) % pieces.len()].as_bytes()[0],
                    }
                }
                if let Ok(mangled) = String::from_utf8(bytes) {
                    let _ = AVRO_OPS.compile(&mangled);
                }
            }
        }
    }

    // -------------------------------------------------------------------------
    // Resource bounds
    // -------------------------------------------------------------------------

    fn quickly<T>(what: &str, ceiling: Duration, run: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let out = run();
        let took = started.elapsed();
        eprintln!("avro bound {what}: {took:?}");
        assert!(took < ceiling, "{what} took {took:?}");
        out
    }

    #[test]
    fn a_declared_length_larger_than_the_payload_is_refused_without_reading() {
        let s = rec(r#"{"name":"s","type":"string"},{"name":"b","type":"bytes"}"#);
        for declared in [i64::MAX, 1 << 62, 1 << 40, 1_000_000] {
            let payload = zz(declared);
            let message = quickly("huge string length", Duration::from_millis(250), || {
                violation(check(&s, &payload))
            });
            assert!(message.contains("/s: declared length exceeds"), "{message}");
        }
        let mut second = text("ok");
        second.extend(zz(1 << 50));
        assert!(violation(check(&s, &second)).contains("/b: declared length exceeds"));
    }

    #[test]
    fn a_block_count_larger_than_the_payload_can_hold_is_refused_at_once() {
        for items in [
            r#""long""#,
            r#""string""#,
            r#"["null","int"]"#,
            r#"{"type":"array","items":"int"}"#,
        ] {
            let s = rec(&format!(
                r#"{{"name":"a","type":{{"type":"array","items":{items}}}}}"#
            ));
            for count in [i64::MAX, 1 << 50, 5_000_000] {
                let payload = zz(count);
                let message = quickly("array count bomb", Duration::from_millis(250), || {
                    violation(check(&s, &payload))
                });
                assert!(
                    message.contains("/a/[]: block declares more items"),
                    "{items}: {message}"
                );
            }
        }
        let map = rec(r#"{"name":"m","type":{"type":"map","values":"null"}}"#);
        assert!(violation(check(&map, &zz(1 << 40))).contains("/m/{}: block declares more items"));
        // i64::MIN as a negative count must not overflow.
        let min = [&zz(i64::MIN)[..], &zz(0)[..]].concat();
        let arr = rec(r#"{"name":"a","type":{"type":"array","items":"int"}}"#);
        assert!(violation(check(&arr, &min)).contains("/a/[]"));
    }

    #[test]
    fn zero_width_items_are_charged_not_looped_over() {
        let nulls = rec(r#"{"name":"a","type":{"type":"array","items":"null"}}"#);
        let empties = rec(
            r#"{"name":"a","type":{"type":"array","items":{"type":"record","name":"E","fields":[]}}}"#,
        );
        let budgeted = |schema: &str, count: i64| {
            let payload = [&zz(count)[..], &zz(0)[..]].concat();
            quickly("zero-width block", Duration::from_secs(2), || {
                check(schema, &payload)
            })
        };
        // Within the work cap these are valid, ...
        assert!(budgeted(&nulls, 10_000_000).is_ok());
        assert!(budgeted(&empties, 1_000_000).is_ok());
        // Zero-width items still cost the batch they arrive in: the fifth
        // document of 15M nulls does not fit the 50M a batch may spend.
        let c = compiled(&nulls);
        let payload = [&zz(15_000_000)[..], &zz(0)[..]].concat();
        let mut batch = ValidationBudget::for_batch("org", 0);
        let verdicts: Vec<_> = (0..5)
            .map(|_| AVRO_OPS.validate_metered(&c, &payload, &mut batch))
            .collect();
        assert!(verdicts[..3].iter().all(Result::is_ok), "{verdicts:?}");
        assert!(
            matches!(verdicts[4], Err(SchemaError::LimitExceeded(_))),
            "{verdicts:?}"
        );
        // ... beyond it the check gives up instead of deciding.
        for count in [30_000_000, 1 << 50, i64::MAX] {
            let message = limit(budgeted(&nulls, count));
            assert!(message.contains("validation work budget"), "{message}");
            assert!(limit(budgeted(&empties, count)).contains("validation work budget"));
        }
    }

    #[test]
    fn many_small_values_cost_the_budget_and_a_batch_shares_it() {
        let ints = rec(r#"{"name":"a","type":{"type":"array","items":"int"}}"#);
        let mut payload = zz(100_000);
        payload.extend(std::iter::repeat(2).take(100_000));
        payload.push(0);
        let c = compiled(&ints);
        let mut shared = ValidationBudget::unshared();
        assert!(AVRO_OPS.validate_metered(&c, &payload, &mut shared).is_ok());

        // A batch allowance too small for one more such document: refused as
        // too complex, not as a violation.
        let mut tight = ValidationBudget::for_batch("org", 0);
        let mut outcomes = Vec::new();
        for _ in 0..700 {
            outcomes.push(AVRO_OPS.validate_metered(&c, &payload, &mut tight));
        }
        assert!(outcomes[0].is_ok());
        assert!(
            outcomes
                .iter()
                .any(|o| matches!(o, Err(SchemaError::LimitExceeded(_)))),
            "700 x 100k values exceed the 50M batch base"
        );
        assert!(outcomes
            .iter()
            .all(|o| !matches!(o, Err(SchemaError::Violation(_)))));
    }

    #[test]
    fn a_wide_union_and_deep_nesting_are_checked_in_linear_time() {
        let enums = (0..MAX_UNION_BRANCHES)
            .map(|i| format!(r#"{{"type":"enum","name":"E{i}","symbols":["A","B"]}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let fields = (0..MAX_RECORD_FIELDS)
            .map(|i| {
                if i == 0 {
                    format!(r#"{{"name":"f0","type":[{enums}]}}"#)
                } else {
                    format!(r#"{{"name":"f{i}","type":"E0"}}"#)
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        let s = rec(&fields);
        let mut payload = Vec::new();
        for i in 0..MAX_RECORD_FIELDS {
            if i == 0 {
                payload.extend(zz(63));
            }
            payload.extend(zz(1));
        }
        assert!(quickly("1024 fields", Duration::from_secs(1), || check(
            &s, &payload
        ))
        .is_ok());
        // Branch 64 does not exist.
        let mut bad = zz(64);
        bad.extend(zz(0));
        assert!(violation(check(&s, &bad)).contains("/f0: union branch index"));
    }

    #[test]
    fn compile_and_compare_adversarial_schemas_within_the_budget() {
        // 1000 named records, each referring to the previous one: the
        // smallest-encoding fixpoint resolves them one id order apart.
        let mut fields = vec![r#"{"name":"f0","type":{"type":"record","name":"C0","fields":[{"name":"x","type":"int"}]}}"#.to_string()];
        for i in 1..1000 {
            fields.push(format!(
                r#"{{"name":"f{i}","type":{{"type":"record","name":"C{i}","fields":[{{"name":"p","type":"C{}"}}]}}}}"#,
                i - 1
            ));
        }
        let chain = rec(&fields.join(","));
        let started = Instant::now();
        let compiled = AVRO_OPS.compile(&chain);
        eprintln!("avro bound chain compile: {:?}", started.elapsed());
        assert!(compiled.is_ok(), "{compiled:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
        // Comparing it with itself walks 1000 levels of references, one pair each.
        let started = Instant::now();
        let verdict = AVRO_OPS.check_compatibility(&chain, &chain, Compatibility::Full);
        eprintln!(
            "avro bound chain compare: {:?} {verdict:?}",
            started.elapsed()
        );
        assert!(matches!(
            verdict,
            Ok(()) | Err(SchemaError::LimitExceeded(_))
        ));
        assert!(started.elapsed() < Duration::from_secs(2));

        // Wide unions of records nested three deep: the pairing work
        // multiplies per level, but the comparison stops at its budget.
        fn build(depth: usize, tag: &str) -> String {
            if depth == 0 {
                return format!(
                    r#"{{"type":"record","name":"Leaf{tag}","fields":[{{"name":"x","type":"int"}}]}}"#
                );
            }
            let branches = (0..4)
                .map(|b| {
                    let fields = (0..4)
                        .map(|f| {
                            format!(
                                r#"{{"name":"f{f}","type":{}}}"#,
                                build(depth - 1, &format!("{tag}{b}{f}"))
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    format!(r#"{{"type":"record","name":"R{tag}{b}d{depth}","fields":[{fields}]}}"#)
                })
                .collect::<Vec<_>>();
            format!("[{}]", branches.join(","))
        }
        let wide = rec(&format!(r#"{{"name":"root","type":{}}}"#, build(2, "x")));
        assert!(AVRO_OPS.compile(&wide).is_ok());
        let started = Instant::now();
        let verdict = AVRO_OPS.check_compatibility(&wide, &wide, Compatibility::Full);
        eprintln!(
            "avro bound wide compare: {:?} {verdict:?}",
            started.elapsed()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(
            verdict,
            Ok(()) | Err(SchemaError::LimitExceeded(_))
        ));
    }

    #[test]
    fn a_comparison_that_runs_out_of_budget_is_not_called_compatible() {
        // 64 records that share the unqualified name `Q` (so they are matched
        // with each other), each with 8 fields that are a union of all the
        // earlier ones: every pair of records pairs 8 unions of up to 64 x 64
        // branches.
        let mut fields = Vec::new();
        for i in 0..64 {
            let earlier: Vec<String> = (0..i).map(|j| format!("\"n{j}.Q\"")).collect();
            let ty = if earlier.is_empty() {
                "\"int\"".to_string()
            } else {
                format!("[\"null\",{}]", earlier.join(","))
            };
            let inner = (0..8)
                .map(|f| format!(r#"{{"name":"f{f}","type":{ty}}}"#))
                .collect::<Vec<_>>()
                .join(",");
            fields.push(format!(
                r#"{{"name":"g{i}","type":{{"type":"record","name":"Q","namespace":"n{i}","fields":[{inner}]}}}}"#
            ));
        }
        let schema = rec(&fields.join(","));
        let other = schema.replace("\"int\"", "\"long\"");
        let started = Instant::now();
        let verdict = AVRO_OPS.check_compatibility(&schema, &other, Compatibility::Full);
        eprintln!(
            "avro bound union compare: {:?} {verdict:?}",
            started.elapsed()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(
            matches!(&verdict, Err(SchemaError::LimitExceeded(m)) if m.contains("too complex to compare")),
            "a comparison that did not finish must not be answered: {verdict:?}"
        );
    }

    // -------------------------------------------------------------------------
    // Compatibility
    // -------------------------------------------------------------------------

    fn reads_text(writer: &str, reader: &str) -> Result<(), String> {
        let (_, w) = parse_text(writer).unwrap();
        let (_, r) = parse_text(reader).unwrap();
        match reads(&w, &r) {
            Ok(()) => Ok(()),
            Err(Stop::No(why)) => Err(why.render("writer", "reader")),
            Err(Stop::Complex) => panic!("too complex"),
        }
    }

    fn library_reads(writer: &str, reader: &str) -> bool {
        let w = Schema::parse_str(writer).unwrap();
        let r = Schema::parse_str(reader).unwrap();
        SchemaCompatibility::can_read(&w, &r) == Ok(LibraryVerdict::Full)
    }

    #[test]
    fn reads_follows_avro_schema_resolution() {
        let r = |fields: &str| rec(fields);
        let a_int = r(r#"{"name":"a","type":"int"}"#);
        let cases: Vec<(String, String, bool, &str)> = vec![
            // Promotions.
            (r#""int""#.into(), r#""long""#.into(), true, "int to long"),
            (r#""long""#.into(), r#""int""#.into(), false, "long to int"),
            (r#""int""#.into(), r#""float""#.into(), true, "int to float"),
            (
                r#""long""#.into(),
                r#""double""#.into(),
                true,
                "long to double",
            ),
            (
                r#""float""#.into(),
                r#""double""#.into(),
                true,
                "float to double",
            ),
            (
                r#""double""#.into(),
                r#""float""#.into(),
                false,
                "double to float",
            ),
            (
                r#""string""#.into(),
                r#""bytes""#.into(),
                true,
                "string to bytes",
            ),
            (
                r#""bytes""#.into(),
                r#""string""#.into(),
                true,
                "bytes to string",
            ),
            (
                r#""boolean""#.into(),
                r#""int""#.into(),
                false,
                "boolean to int",
            ),
            (r#""null""#.into(), r#""null""#.into(), true, "null"),
            (
                r#""int""#.into(),
                r#""string""#.into(),
                false,
                "int to string",
            ),
            // Containers.
            (
                r#"{"type":"array","items":"int"}"#.into(),
                r#"{"type":"array","items":"long"}"#.into(),
                true,
                "array promotion",
            ),
            (
                r#"{"type":"map","values":"long"}"#.into(),
                r#"{"type":"map","values":"int"}"#.into(),
                false,
                "map narrowing",
            ),
            (
                r#"{"type":"array","items":"int"}"#.into(),
                r#"{"type":"map","values":"int"}"#.into(),
                false,
                "array to map",
            ),
            // Records.
            (
                a_int.clone(),
                r(r#"{"name":"a","type":"int"},{"name":"b","type":"string","default":""}"#),
                true,
                "reader adds a field with a default",
            ),
            (
                a_int.clone(),
                r(r#"{"name":"a","type":"int"},{"name":"b","type":"string"}"#),
                false,
                "reader adds a field without a default",
            ),
            (
                r(r#"{"name":"a","type":"int"},{"name":"b","type":"string"}"#),
                a_int.clone(),
                true,
                "reader drops a field",
            ),
            (
                a_int.clone(),
                r(r#"{"name":"a","type":"long"}"#),
                true,
                "field promotion",
            ),
            (
                r(r#"{"name":"a","type":"long"}"#),
                a_int.clone(),
                false,
                "field narrowing",
            ),
            (
                a_int.clone(),
                r(r#"{"name":"c","type":"int","aliases":["a"]}"#),
                true,
                "reader alias names the writer field",
            ),
            (
                r(r#"{"name":"c","type":"int","aliases":["a"]}"#),
                a_int.clone(),
                false,
                "writer alias never matches",
            ),
            (
                a_int.clone(),
                a_int.replace("\"R\"", "\"S\""),
                false,
                "different record names",
            ),
            (
                a_int.clone(),
                a_int.replace("\"name\":\"R\"", "\"name\":\"R\",\"namespace\":\"n.s\""),
                true,
                "namespaces are ignored",
            ),
            (
                a_int.clone(),
                a_int.replace("\"name\":\"R\"", "\"name\":\"S\",\"aliases\":[\"R\"]"),
                true,
                "reader type alias",
            ),
            // Enums and fixed.
            (
                r#"{"type":"enum","name":"E","symbols":["A","B"]}"#.into(),
                r#"{"type":"enum","name":"E","symbols":["A","B","C"]}"#.into(),
                true,
                "enum gains a symbol",
            ),
            (
                r#"{"type":"enum","name":"E","symbols":["A","B","C"]}"#.into(),
                r#"{"type":"enum","name":"E","symbols":["A","B"]}"#.into(),
                false,
                "enum loses a symbol",
            ),
            (
                r#"{"type":"enum","name":"E","symbols":["A","B","C"]}"#.into(),
                r#"{"type":"enum","name":"E","symbols":["A"],"default":"A"}"#.into(),
                true,
                "enum default",
            ),
            (
                r#"{"type":"fixed","name":"F","size":4}"#.into(),
                r#"{"type":"fixed","name":"F","size":4}"#.into(),
                true,
                "fixed same size",
            ),
            (
                r#"{"type":"fixed","name":"F","size":4}"#.into(),
                r#"{"type":"fixed","name":"F","size":8}"#.into(),
                false,
                "fixed other size",
            ),
            // Unions.
            (
                r#""string""#.into(),
                r#"["null","string"]"#.into(),
                true,
                "reader wraps in a union",
            ),
            (
                r#"["null","string"]"#.into(),
                r#""string""#.into(),
                false,
                "reader unwraps a union (null may be written)",
            ),
            (
                r#"["null","string"]"#.into(),
                r#"["null","string","int"]"#.into(),
                true,
                "union gains a branch",
            ),
            (
                r#"["string","int"]"#.into(),
                r#"["string"]"#.into(),
                false,
                "union loses a branch",
            ),
            (
                r#"["int"]"#.into(),
                r#"["long","string"]"#.into(),
                true,
                "union promotion",
            ),
            // Decimals.
            (
                r#"{"type":"bytes","logicalType":"decimal","precision":6,"scale":2}"#.into(),
                r#"{"type":"bytes","logicalType":"decimal","precision":6,"scale":2}"#.into(),
                true,
                "same decimal",
            ),
            (
                r#"{"type":"bytes","logicalType":"decimal","precision":6,"scale":2}"#.into(),
                r#"{"type":"bytes","logicalType":"decimal","precision":8,"scale":2}"#.into(),
                false,
                "other decimal precision",
            ),
        ];
        for (writer, reader, expected, what) in &cases {
            let mine = reads_text(writer, reader);
            assert_eq!(mine.is_ok(), *expected, "{what}: {mine:?}");
            // The library ignores aliases of types, so it is no oracle there.
            if !what.contains("type alias") {
                assert_eq!(
                    library_reads(writer, reader),
                    *expected,
                    "{what}: the library disagrees, so this case is not an oracle"
                );
            }
        }
    }

    #[test]
    fn reads_resolves_named_types_by_identity_not_by_position() {
        // The same Address, defined in `billing` in one version and in
        // `shipping` in the other; `can_read` calls a Ref against an inline
        // definition a type mismatch.
        let address =
            r#"{"type":"record","name":"Address","fields":[{"name":"zip","type":"string"}]}"#;
        let v1 = rec(&format!(
            r#"{{"name":"billing","type":{address}}},{{"name":"shipping","type":"Address"}}"#
        ));
        // The same fields in the other order, so the definition now sits in `shipping`.
        let reordered = rec(&format!(
            r#"{{"name":"shipping","type":{address}}},{{"name":"billing","type":"Address"}}"#
        ));
        assert!(
            !library_reads(&v1, &reordered),
            "library oracle: false incompatibility"
        );
        assert!(reads_text(&v1, &reordered).is_ok());
        assert!(reads_text(&reordered, &v1).is_ok());
        // ... and a real change inside the shared type is still found.
        let changed = reordered.replace(
            r#""name":"zip","type":"string""#,
            r#""name":"zip","type":"int""#,
        );
        assert!(reads_text(&v1, &changed).is_err());
    }

    #[test]
    fn recursive_types_compare_to_a_fixpoint() {
        let with_label = LIST.replace(
            r#""fields":["#,
            r#""fields":[{"name":"label","type":"string","default":""},"#,
        );
        assert!(reads_text(LIST, &with_label).is_ok());
        let required_label = LIST.replace(
            r#""fields":["#,
            r#""fields":[{"name":"label","type":"string"},"#,
        );
        let why = reads_text(LIST, &required_label).unwrap_err();
        assert!(
            why.contains("requires field 'label' of record 'L'"),
            "{why}"
        );
        // The failure sits at the root, not under `next`.
        assert!(why.starts_with("/label:"), "{why}");
        // A change deep in the recursion surfaces through the cycle.
        let narrowed = LIST.replace(
            r#""name":"v","type":"int""#,
            r#""name":"v","type":"string""#,
        );
        assert!(reads_text(LIST, &narrowed).is_err());
        assert!(reads_text(LIST, LIST).is_ok());
    }

    #[test]
    fn modes_map_to_the_right_direction() {
        let old = rec(r#"{"name":"a","type":"int"}"#);
        let with_default =
            rec(r#"{"name":"a","type":"int"},{"name":"b","type":"string","default":""}"#);
        let required = rec(r#"{"name":"a","type":"int"},{"name":"b","type":"string"}"#);
        let widened = rec(r#"{"name":"a","type":"long"}"#);
        let check = |old: &str, new: &str, mode| AVRO_OPS.check_compatibility(old, new, mode);
        use Compatibility::{Backward, Forward, Full, None as NoCheck};

        // Optional field with a default: both directions.
        for mode in [Backward, Forward, Full, NoCheck] {
            assert!(check(&old, &with_default, mode).is_ok(), "{mode:?}");
        }
        // New required field: the new reader cannot read old data (backward);
        // the old reader ignores it (forward).
        let backward = check(&old, &required, Backward).unwrap_err();
        assert!(
            matches!(&backward, SchemaError::Incompatible(m)
            if m.starts_with("backward: data written under the old schema")
            && m.contains("the new schema requires field 'b' of record 'R'")
            && m.contains("missing from the old schema")),
            "{backward:?}"
        );
        assert!(check(&old, &required, Forward).is_ok());
        assert!(check(&old, &required, Full).is_err());
        // Removing that field again is the mirror image.
        assert!(check(&required, &old, Backward).is_ok());
        let forward = check(&required, &old, Forward).unwrap_err();
        assert!(
            matches!(&forward, SchemaError::Incompatible(m)
            if m.starts_with("forward: data written under the new schema")
            && m.contains("the old schema requires field 'b'")
            && m.contains("missing from the new schema")),
            "{forward:?}"
        );
        // Promotion int -> long: new readers read old ints; old int readers cannot read longs.
        assert!(check(&old, &widened, Backward).is_ok());
        assert!(check(&old, &widened, Forward).is_err());
        assert!(check(&old, &widened, Full).is_err());
        assert!(check(&old, &widened, NoCheck).is_ok());
        // A path names the field that broke.
        let nested_old = rec(
            r#"{"name":"inner","type":{"type":"record","name":"I","fields":[{"name":"n","type":"long"}]}}"#,
        );
        let nested_new = nested_old.replace("\"long\"", "\"int\"");
        let Err(SchemaError::Incompatible(m)) = check(&nested_old, &nested_new, Backward) else {
            panic!("narrowing must be refused");
        };
        assert!(
            m.contains(
                "/inner/n: long written by the old schema cannot be read as int by the new schema"
            ),
            "{m}"
        );
    }

    #[test]
    fn compatibility_checks_parse_with_the_same_limits_as_compile() {
        let ok = rec(r#"{"name":"a","type":"int"}"#);
        assert!(matches!(
            AVRO_OPS.check_compatibility("nope", &ok, Compatibility::Backward),
            Err(SchemaError::Invalid(_))
        ));
        let fields = (0..MAX_RECORD_FIELDS + 1)
            .map(|i| format!(r#"{{"name":"f{i}","type":"int"}}"#))
            .collect::<Vec<_>>()
            .join(",");
        assert!(matches!(
            AVRO_OPS.check_compatibility(&ok, &rec(&fields), Compatibility::Forward),
            Err(SchemaError::Invalid(_))
        ));
    }

    // -------------------------------------------------------------------------
    // Projection
    // -------------------------------------------------------------------------

    fn allowed(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn derive_keeps_the_allowed_top_level_fields_in_order() {
        let order = fixtures::read("avro", "order.avsc");
        let derived = AVRO_OPS
            .derive_subschema(&order, &allowed(&["price", "id", "lines", "nope"]))
            .unwrap();
        let value: Value = serde_json::from_str(&derived).unwrap();
        let names: Vec<&str> = value["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["id", "lines", "price"],
            "schema order, unknown names ignored"
        );
        assert_eq!(value["name"], "Order");
        assert_eq!(value["namespace"], "shop");
        assert_eq!(value["doc"], "A shop order");
        // Deterministic.
        assert_eq!(
            derived,
            AVRO_OPS
                .derive_subschema(&order, &allowed(&["nope", "lines", "id", "price"]))
                .unwrap()
        );
        // Registrable, and old data reads under it (the projection drops fields only).
        assert!(AVRO_OPS.compile(&derived).is_ok());
        assert!(AVRO_OPS
            .check_compatibility(&order, &derived, Compatibility::Backward)
            .is_ok());
        // Everything allowed: the same schema; nothing allowed: an empty record.
        let all = AVRO_OPS
            .derive_subschema(
                &order,
                &allowed(&[
                    "id", "customer", "paid", "status", "note", "lines", "attrs", "checksum",
                    "price",
                ]),
            )
            .unwrap();
        assert!(AVRO_OPS
            .check_compatibility(&order, &all, Compatibility::Full)
            .is_ok());
        let none = AVRO_OPS.derive_subschema(&order, &allowed(&[])).unwrap();
        assert!(none.contains(r#""fields":[]"#));
        assert!(check(&none, b"").is_ok());
    }

    #[test]
    fn derive_hoists_a_definition_out_of_a_dropped_field() {
        let address =
            r#"{"type":"record","name":"Address","fields":[{"name":"zip","type":"string"}]}"#;
        let schema = rec(&format!(
            r#"{{"name":"billing","type":{address}}},{{"name":"shipping","type":["null","Address"],"default":null}},{{"name":"again","type":"Address"}}"#
        ));
        // `billing` carries the definition and is dropped: `shipping` takes it over.
        let derived = AVRO_OPS
            .derive_subschema(&schema, &allowed(&["shipping", "again"]))
            .unwrap();
        assert_eq!(
            derived.matches("\"name\":\"Address\"").count(),
            1,
            "{derived}"
        );
        assert!(AVRO_OPS.compile(&derived).is_ok(), "{derived}");
        let mut datum = zz(1);
        datum.extend(text("00-001"));
        datum.extend(text("11-111"));
        assert!(check(&derived, &datum).is_ok());
        // Dropping a recursive type's holder keeps the cycle intact.
        let tree = rec(&format!(
            r#"{{"name":"list","type":{LIST}}},{{"name":"v","type":"int"}}"#
        ));
        let only_list = AVRO_OPS
            .derive_subschema(&tree, &allowed(&["list"]))
            .unwrap();
        assert!(AVRO_OPS.compile(&only_list).is_ok());
    }

    #[test]
    fn derive_refuses_a_root_that_is_not_a_record() {
        assert!(matches!(
            AVRO_OPS.derive_subschema(r#""string""#, &allowed(&["a"])),
            Err(SchemaError::Invalid(m)) if m.contains("must be a record")
        ));
        assert!(matches!(
            AVRO_OPS.derive_subschema("nope", &allowed(&[])),
            Err(SchemaError::Invalid(_))
        ));
    }

    // -------------------------------------------------------------------------
    // Wiring
    // -------------------------------------------------------------------------

    #[test]
    fn avro_is_a_validating_kind_bound_independently_of_content_type() {
        assert!(SchemaType::Avro.has_validator());
        assert_eq!(SchemaType::Avro.required_payload_format(), None);
        let foreign = AVRO_OPS.validate(&SchemaType::JsonSchema.ops().compile("{}").unwrap(), b"");
        assert!(matches!(foreign, Err(SchemaError::Invalid(_))));
    }
}
