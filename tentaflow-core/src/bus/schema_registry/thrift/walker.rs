// =============================================================================
// File: bus/schema_registry/thrift/walker.rs — iterative TBinary/TCompact payload walk
// =============================================================================
// One serialized root struct in, a verdict out. Both wire protocols run
// through the same loop: the protocol only decides how a header, a length and
// a number are read. The walk has an explicit stack of at most
// `MAX_VALUE_DEPTH` frames (a struct, a list or set, a map) and allocates
// nothing per value, so a payload cannot exhaust the native stack or memory,
// whatever it nests or declares. See the header of the parent module for what
// is accepted and what is refused.
//
// Order of checks for anything of variable size: the declared length or count
// is compared with the bytes that remain (a count also with its element's
// minimum size) and charged to the budget BEFORE any byte is skipped or any
// element iterated.
// =============================================================================

use super::{
    Base, Budget, Compiled, ExprId, LimitExceeded, Protocol, SchemaError, Shape, MAX_VALUE_DEPTH,
};
use crate::bus::schema_registry::shorten_path;

const TRUNCATED: &str = "payload ends before the value is complete";
const TRAILING: &str = "bytes remain after the value";
const INT_TOO_LONG: &str = "integer encoding is longer than allowed or overflows its type";
const LENGTH_EXCEEDS: &str = "declared length exceeds the remaining payload";
const COUNT_EXCEEDS: &str = "declared size exceeds the remaining payload";
const NEGATIVE: &str = "length or size is negative";
const BAD_UTF8: &str = "string is not valid UTF-8";
const BAD_BOOLEAN: &str = "boolean is not a valid value";
const BAD_TYPE: &str = "wire type is not valid";
const FIELD_ID: &str = "field id is out of range";
const WIRE_MISMATCH: &str = "wire type does not match the type declared for the field";
const ELEMENT_MISMATCH: &str = "element wire type does not match the type declared for the field";
const ENUM_UNDECLARED: &str = "enum value is not declared in the enum";
const REQUIRED_MISSING: &str = "required field is missing";
const UNION_MEMBERS: &str = "more than one member of the union is set";
const TOO_DEEP: &str = "value is nested too deeply to check";
const VALIDATION_BUDGET: &str = "document exceeds the validation work budget";

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

/// The wire type of a value, whichever protocol carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Wt {
    Bool,
    Byte,
    Double,
    I16,
    I32,
    I64,
    Binary,
    Struct,
    Map,
    Set,
    List,
    Uuid,
}

impl Wt {
    fn from_binary(code: u8) -> Option<Wt> {
        Some(match code {
            2 => Wt::Bool,
            3 => Wt::Byte,
            4 => Wt::Double,
            6 => Wt::I16,
            8 => Wt::I32,
            10 => Wt::I64,
            11 => Wt::Binary,
            12 => Wt::Struct,
            13 => Wt::Map,
            14 => Wt::Set,
            15 => Wt::List,
            16 => Wt::Uuid,
            _ => return None,
        })
    }

    /// Both codes of a compact boolean (1 and 2) are a `bool`.
    fn from_compact(code: u8) -> Option<Wt> {
        Some(match code {
            1 | 2 => Wt::Bool,
            3 => Wt::Byte,
            4 => Wt::I16,
            5 => Wt::I32,
            6 => Wt::I64,
            7 => Wt::Double,
            8 => Wt::Binary,
            9 => Wt::List,
            10 => Wt::Set,
            11 => Wt::Map,
            12 => Wt::Struct,
            13 => Wt::Uuid,
            _ => return None,
        })
    }

    /// The fewest bytes one value of this type takes on the wire.
    fn min_size(self, compact: bool) -> u64 {
        match self {
            Wt::Bool | Wt::Byte | Wt::Struct => 1,
            Wt::Double => 8,
            Wt::Uuid => 16,
            Wt::I16 if !compact => 2,
            Wt::I32 if !compact => 4,
            Wt::I64 if !compact => 8,
            Wt::Binary if !compact => 4,
            Wt::List | Wt::Set if !compact => 5,
            Wt::Map if !compact => 6,
            _ => 1,
        }
    }
}

fn expected(shape: Shape) -> Wt {
    match shape {
        Shape::Base(Base::Bool) => Wt::Bool,
        Shape::Base(Base::Byte) => Wt::Byte,
        Shape::Base(Base::I16) => Wt::I16,
        Shape::Base(Base::I32) | Shape::Enum(_) => Wt::I32,
        Shape::Base(Base::I64) => Wt::I64,
        Shape::Base(Base::Double) => Wt::Double,
        Shape::Base(Base::String | Base::Binary) => Wt::Binary,
        Shape::Base(Base::Uuid) => Wt::Uuid,
        Shape::Struct(_) => Wt::Struct,
        Shape::List(_) => Wt::List,
        Shape::Set(_) => Wt::Set,
        Shape::Map(..) => Wt::Map,
    }
}

/// What a value is checked against: a type of the schema, or nothing (a field
/// the schema does not declare, whose framing is all that is checked).
#[derive(Clone, Copy)]
enum Target {
    Known(ExprId),
    Unknown,
}

/// How a value was reached from the frame below it, for the path in a message.
#[derive(Clone, Copy)]
enum Seg {
    Root,
    Field(usize, usize),
    UnknownField,
    Element,
    Key,
    Value,
}

#[derive(Clone, Copy)]
enum Cursor {
    Struct {
        def: Option<usize>,
        /// `required` fields met so far.
        seen: u64,
        /// Known members met so far (a union allows one).
        members: u32,
        /// Compact field-id deltas count from here.
        last_id: i32,
    },
    Seq {
        elem: Target,
        wt: Wt,
        remaining: u64,
    },
    Map {
        key: Target,
        key_wt: Wt,
        value: Target,
        value_wt: Wt,
        remaining: u64,
        key_next: bool,
    },
}

struct Frame {
    cursor: Cursor,
    via: Seg,
}

struct Walker<'a> {
    c: &'a Compiled,
    data: &'a [u8],
    pos: usize,
    compact: bool,
    budget: &'a mut Budget,
    stack: Vec<Frame>,
    /// What the failing operation was reading, appended to the path.
    leaf: Option<Seg>,
}

impl<'a> Walker<'a> {
    fn left(&self) -> usize {
        self.data.len() - self.pos
    }

    fn take(&mut self, n: usize) -> Walk<&'a [u8]> {
        let data: &'a [u8] = self.data;
        let chunk = data
            .get(self.pos..)
            .and_then(|rest| rest.get(..n))
            .ok_or(Fail::Violation(TRUNCATED))?;
        self.pos += n;
        Ok(chunk)
    }

    fn byte(&mut self) -> Walk<u8> {
        Ok(self.take(1)?[0])
    }

    fn be_i16(&mut self) -> Walk<i16> {
        let b = self.take(2)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }

    fn be_i32(&mut self) -> Walk<i32> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// A compact varint of at most `max_bytes` bytes that does not exceed `limit`.
    fn varint(&mut self, max_bytes: usize, limit: u64) -> Walk<u64> {
        let mut value = 0u64;
        for i in 0..max_bytes {
            let byte = self.byte()?;
            // The tenth byte may only carry bit 63.
            if i == 9 && byte > 1 {
                return Err(Fail::Violation(INT_TOO_LONG));
            }
            value |= u64::from(byte & 0x7f) << (7 * i);
            if byte & 0x80 == 0 {
                return if value <= limit {
                    Ok(value)
                } else {
                    Err(Fail::Violation(INT_TOO_LONG))
                };
            }
        }
        Err(Fail::Violation(INT_TOO_LONG))
    }

    /// A count or length: non-negative, in the range of an i32.
    fn size(&mut self) -> Walk<u64> {
        if self.compact {
            let n = self.varint(5, u64::from(u32::MAX))?;
            if n > i32::MAX as u64 {
                return Err(Fail::Violation(NEGATIVE));
            }
            Ok(n)
        } else {
            let n = self.be_i32()?;
            u64::try_from(n).map_err(|_| Fail::Violation(NEGATIVE))
        }
    }

    fn push(&mut self, cursor: Cursor, via: Seg) -> Walk<()> {
        if self.stack.len() >= MAX_VALUE_DEPTH {
            return Err(Fail::Limit(TOO_DEEP));
        }
        self.stack.push(Frame { cursor, via });
        Ok(())
    }

    fn enum_check(&self, e: usize, value: i32) -> Walk<()> {
        match self.c.enums.get(e) {
            Some(def) if def.numbers.binary_search(&value).is_ok() => Ok(()),
            _ => Err(Fail::Violation(ENUM_UNDECLARED)),
        }
    }

    // -------------------------------------------------------------------------
    // The loop
    // -------------------------------------------------------------------------

    fn run(&mut self) -> Walk<()> {
        self.push(
            Cursor::Struct {
                def: Some(self.c.root),
                seen: 0,
                members: 0,
                last_id: 0,
            },
            Seg::Root,
        )?;
        while let Some(frame) = self.stack.last() {
            let cursor = frame.cursor;
            self.leaf = None;
            match cursor {
                Cursor::Struct {
                    def,
                    seen,
                    members,
                    last_id,
                } => self.struct_step(def, seen, members, last_id)?,
                Cursor::Seq {
                    elem,
                    wt,
                    remaining,
                } => {
                    if remaining == 0 {
                        self.stack.pop();
                        continue;
                    }
                    self.budget.charge(1)?;
                    self.set_cursor(Cursor::Seq {
                        elem,
                        wt,
                        remaining: remaining - 1,
                    });
                    self.leaf = Some(Seg::Element);
                    self.value(elem, wt, None)?;
                }
                Cursor::Map {
                    key,
                    key_wt,
                    value,
                    value_wt,
                    remaining,
                    key_next,
                } => {
                    if key_next && remaining == 0 {
                        self.stack.pop();
                        continue;
                    }
                    self.budget.charge(1)?;
                    let (target, wt, via, remaining) = if key_next {
                        (key, key_wt, Seg::Key, remaining)
                    } else {
                        (value, value_wt, Seg::Value, remaining - 1)
                    };
                    self.set_cursor(Cursor::Map {
                        key,
                        key_wt,
                        value,
                        value_wt,
                        remaining,
                        key_next: !key_next,
                    });
                    self.leaf = Some(via);
                    self.value(target, wt, None)?;
                }
            }
        }
        self.leaf = None;
        if self.pos != self.data.len() {
            return Err(Fail::Violation(TRAILING));
        }
        Ok(())
    }

    fn set_cursor(&mut self, cursor: Cursor) {
        if let Some(frame) = self.stack.last_mut() {
            frame.cursor = cursor;
        }
    }

    /// The next field header of the innermost struct, or its end.
    fn struct_step(
        &mut self,
        def: Option<usize>,
        seen: u64,
        members: u32,
        last_id: i32,
    ) -> Walk<()> {
        self.budget.charge(1)?;
        let Some((wt, id, inline)) = self.field_header(last_id)? else {
            return self.close_struct(def, seen);
        };
        let known = def.and_then(|d| {
            let index = self.c.structs.get(d)?.field_by_id(id)?;
            Some((d, index))
        });
        let (mut seen, mut members) = (seen, members);
        let mut target = Target::Unknown;
        if let Some((d, index)) = known {
            self.leaf = Some(Seg::Field(d, index));
            let def = &self.c.structs[d];
            let field = &def.fields[index];
            if let Some(bit) = field.required_bit {
                seen |= 1u64 << bit;
            }
            members += 1;
            if def.kind == super::Kind::Union && members > 1 {
                return Err(Fail::Violation(UNION_MEMBERS));
            }
            if expected(self.c.shapes[field.ty]) != wt {
                return Err(Fail::Violation(WIRE_MISMATCH));
            }
            target = Target::Known(field.ty);
        } else {
            self.leaf = Some(Seg::UnknownField);
        }
        self.set_cursor(Cursor::Struct {
            def,
            seen,
            members,
            last_id: id,
        });
        self.value(target, wt, inline)
    }

    /// Ends the innermost struct: its `required` fields must all have come.
    fn close_struct(&mut self, def: Option<usize>, seen: u64) -> Walk<()> {
        if let Some(d) = def {
            let missing = self.c.structs[d].required_mask & !seen;
            if missing != 0 {
                let bit = missing.trailing_zeros();
                let index = self.c.structs[d]
                    .fields
                    .iter()
                    .position(|f| f.required_bit == Some(bit));
                self.leaf = index.map(|i| Seg::Field(d, i));
                return Err(Fail::Violation(REQUIRED_MISSING));
            }
        }
        self.stack.pop();
        Ok(())
    }

    /// `(type, id, inline bool)` of the next field, or `None` at the stop byte.
    fn field_header(&mut self, last_id: i32) -> Walk<Option<(Wt, i32, Option<bool>)>> {
        if !self.compact {
            let code = self.byte()?;
            if code == 0 {
                return Ok(None);
            }
            let wt = Wt::from_binary(code).ok_or(Fail::Violation(BAD_TYPE))?;
            let id = self.be_i16()?;
            return Ok(Some((wt, i32::from(id), None)));
        }
        let head = self.byte()?;
        if head == 0 {
            return Ok(None);
        }
        let code = head & 0x0f;
        let wt = Wt::from_compact(code).ok_or(Fail::Violation(BAD_TYPE))?;
        let inline = match code {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        };
        let delta = i32::from(head >> 4);
        let id = if delta == 0 {
            let raw = self.varint(3, u64::from(u16::MAX))? as u32;
            i32::from(((raw >> 1) as i32 ^ -((raw & 1) as i32)) as i16)
        } else {
            let id = last_id + delta;
            if id > i32::from(i16::MAX) {
                return Err(Fail::Violation(FIELD_ID));
            }
            id
        };
        Ok(Some((wt, id, inline)))
    }

    // -------------------------------------------------------------------------
    // Values
    // -------------------------------------------------------------------------

    /// One value of wire type `wt`. A scalar is consumed here; a struct, list,
    /// set or map pushes the frame that walks it.
    fn value(&mut self, target: Target, wt: Wt, inline: Option<bool>) -> Walk<()> {
        let shape = match target {
            Target::Known(e) => self.c.shapes.get(e).copied(),
            Target::Unknown => None,
        };
        match wt {
            Wt::Bool => {
                if inline.is_none() {
                    let b = self.byte()?;
                    let valid = if self.compact {
                        b == 1 || b == 2
                    } else {
                        b <= 1
                    };
                    if !valid {
                        return Err(Fail::Violation(BAD_BOOLEAN));
                    }
                }
                Ok(())
            }
            Wt::Byte => self.take(1).map(drop),
            Wt::Double => self.take(8).map(drop),
            Wt::Uuid => self.take(16).map(drop),
            Wt::I16 => {
                if self.compact {
                    self.varint(3, u64::from(u16::MAX)).map(drop)
                } else {
                    self.take(2).map(drop)
                }
            }
            Wt::I32 => {
                let value = if self.compact {
                    let raw = self.varint(5, u64::from(u32::MAX))? as u32;
                    (raw >> 1) as i32 ^ -((raw & 1) as i32)
                } else {
                    self.be_i32()?
                };
                match shape {
                    Some(Shape::Enum(e)) => self.enum_check(e, value),
                    _ => Ok(()),
                }
            }
            Wt::I64 => {
                if self.compact {
                    self.varint(10, u64::MAX).map(drop)
                } else {
                    self.take(8).map(drop)
                }
            }
            Wt::Binary => {
                let len = self.size()?;
                if len > self.left() as u64 {
                    return Err(Fail::Violation(LENGTH_EXCEEDS));
                }
                let len = len as usize;
                self.budget.charge(len as u64 / 32)?;
                let bytes = self.take(len)?;
                if matches!(shape, Some(Shape::Base(Base::String))) {
                    std::str::from_utf8(bytes).map_err(|_| Fail::Violation(BAD_UTF8))?;
                }
                Ok(())
            }
            Wt::Struct => {
                let def = match shape {
                    Some(Shape::Struct(s)) => Some(s),
                    _ => None,
                };
                let via = self.leaf.unwrap_or(Seg::Root);
                self.push(
                    Cursor::Struct {
                        def,
                        seen: 0,
                        members: 0,
                        last_id: 0,
                    },
                    via,
                )
            }
            Wt::List | Wt::Set => self.sequence(shape),
            Wt::Map => self.map(shape),
        }
    }

    /// The bytes a run of `count` elements of `wt` takes when each is a
    /// fixed-size value that needs no look: skipped in one step.
    fn fixed_run(&self, wt: Wt, elem: Target) -> Option<u64> {
        let is_enum = matches!(
            elem,
            Target::Known(e) if matches!(self.c.shapes.get(e), Some(Shape::Enum(_)))
        );
        match wt {
            Wt::Byte => Some(1),
            Wt::Double => Some(8),
            Wt::Uuid => Some(16),
            Wt::I16 if !self.compact => Some(2),
            Wt::I64 if !self.compact => Some(8),
            Wt::I32 if !self.compact && !is_enum => Some(4),
            _ => None,
        }
    }

    /// A target for the elements of a container type, checked against the
    /// element type the header declares.
    fn element(&self, child: ExprId, wt: Wt) -> Walk<Target> {
        match self.c.shapes.get(child) {
            Some(shape) if expected(*shape) == wt => Ok(Target::Known(child)),
            _ => Err(Fail::Violation(ELEMENT_MISMATCH)),
        }
    }

    fn element_type(&self, code: u8) -> Walk<Wt> {
        let wt = if self.compact {
            Wt::from_compact(code)
        } else {
            Wt::from_binary(code)
        };
        wt.ok_or(Fail::Violation(BAD_TYPE))
    }

    fn sequence(&mut self, shape: Option<Shape>) -> Walk<()> {
        let (code, count) = if self.compact {
            let head = self.byte()?;
            let count = match head >> 4 {
                15 => self.size()?,
                n => u64::from(n),
            };
            (head & 0x0f, count)
        } else {
            let code = self.byte()?;
            (code, self.size()?)
        };
        // The element types of an empty container say nothing: writers differ.
        if count == 0 {
            return Ok(());
        }
        let wt = self.element_type(code)?;
        let elem = match shape {
            Some(Shape::List(child) | Shape::Set(child)) => self.element(child, wt)?,
            _ => Target::Unknown,
        };
        let size = wt.min_size(self.compact);
        if count.saturating_mul(size) > self.left() as u64 {
            return Err(Fail::Violation(COUNT_EXCEEDS));
        }
        if let Some(each) = self.fixed_run(wt, elem) {
            let run = count.saturating_mul(each);
            if run > self.left() as u64 {
                return Err(Fail::Violation(COUNT_EXCEEDS));
            }
            self.budget.charge(count / 8 + 1)?;
            self.pos += run as usize;
            return Ok(());
        }
        let via = self.leaf.unwrap_or(Seg::Root);
        self.push(
            Cursor::Seq {
                elem,
                wt,
                remaining: count,
            },
            via,
        )
    }

    fn map(&mut self, shape: Option<Shape>) -> Walk<()> {
        let (key_code, value_code, count) = if self.compact {
            let count = self.size()?;
            if count == 0 {
                return Ok(());
            }
            let types = self.byte()?;
            (types >> 4, types & 0x0f, count)
        } else {
            let key_code = self.byte()?;
            let value_code = self.byte()?;
            (key_code, value_code, self.size()?)
        };
        if count == 0 {
            return Ok(());
        }
        let key_wt = self.element_type(key_code)?;
        let value_wt = self.element_type(value_code)?;
        let (key, value) = match shape {
            Some(Shape::Map(k, v)) => (self.element(k, key_wt)?, self.element(v, value_wt)?),
            _ => (Target::Unknown, Target::Unknown),
        };
        let pair = key_wt.min_size(self.compact) + value_wt.min_size(self.compact);
        if count.saturating_mul(pair) > self.left() as u64 {
            return Err(Fail::Violation(COUNT_EXCEEDS));
        }
        let via = self.leaf.unwrap_or(Seg::Root);
        self.push(
            Cursor::Map {
                key,
                key_wt,
                value,
                value_wt,
                remaining: count,
                key_next: true,
            },
            via,
        )
    }

    // -------------------------------------------------------------------------
    // Messages
    // -------------------------------------------------------------------------

    fn segment(&self, seg: Seg, out: &mut String) {
        match seg {
            Seg::Root => {}
            Seg::Field(d, i) => {
                out.push('/');
                if let Some(field) = self.c.structs.get(d).and_then(|s| s.fields.get(i)) {
                    out.push_str(&field.name);
                }
            }
            Seg::UnknownField => out.push_str("/<unknown>"),
            Seg::Element => out.push_str("/[]"),
            Seg::Key => out.push_str("/<key>"),
            Seg::Value => out.push_str("/<value>"),
        }
    }

    fn render_path(&self) -> String {
        let mut out = String::new();
        for frame in self.stack.iter().skip(1) {
            self.segment(frame.via, &mut out);
        }
        if let Some(leaf) = self.leaf {
            self.segment(leaf, &mut out);
        }
        if out.is_empty() {
            out.push_str("<root>");
        }
        out
    }
}

pub(super) fn validate_with(
    c: &Compiled,
    payload: &[u8],
    budget: &mut Budget,
) -> Result<(), SchemaError> {
    let mut walker = Walker {
        c,
        data: payload,
        pos: 0,
        compact: c.protocol == Protocol::Compact,
        budget,
        stack: Vec::new(),
        leaf: None,
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
