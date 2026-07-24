// ===== File: proto.rs — ONNX protobuf wire-format parser (loader trust boundary) =====
//
// A model file is untrusted input (SPEC §9.5). This module decodes the raw
// protobuf wire format for the ONNX message subset FORGE's executor needs —
// ModelProto / GraphProto / NodeProto / AttributeProto / TensorProto — with
// every length and offset bounds-checked. Parse failures surface as
// `ForgeError::Format`; there are no panics on malformed input.
//
// The decoded structs ARE the graph IR the interpreter walks (nodes, edges by
// tensor name, initializers, attributes, nested subgraphs) — a small, real IR,
// not a framework. Field numbers follow the canonical onnx.proto schema.

use forge_types::{ForgeError, Result};

/// A cursor over protobuf-encoded bytes with checked reads.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

/// Protobuf wire types.
const WIRE_VARINT: u64 = 0;
const WIRE_I64: u64 = 1;
const WIRE_LEN: u64 = 2;
const WIRE_I32: u64 = 5;

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn read_varint(&mut self) -> Result<u64> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            if self.pos >= self.buf.len() {
                return Err(ForgeError::Format("onnx: varint runs past end".into()));
            }
            let byte = self.buf[self.pos];
            self.pos += 1;
            if shift >= 64 {
                return Err(ForgeError::Format("onnx: varint too long".into()));
            }
            result |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        Ok(result)
    }

    /// Returns (field_number, wire_type).
    fn read_tag(&mut self) -> Result<(u64, u64)> {
        let key = self.read_varint()?;
        Ok((key >> 3, key & 0x7))
    }

    fn read_len_bytes(&mut self) -> Result<&'a [u8]> {
        let len = self.read_varint()? as usize;
        let end = self
            .pos
            .checked_add(len)
            .filter(|e| *e <= self.buf.len())
            .ok_or_else(|| ForgeError::Format("onnx: length-delimited field past end".into()))?;
        let slice = &self.buf[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn read_i32(&mut self) -> Result<u32> {
        let end = self.pos + 4;
        if end > self.buf.len() {
            return Err(ForgeError::Format("onnx: fixed32 past end".into()));
        }
        let v = u32::from_le_bytes(self.buf[self.pos..end].try_into().unwrap());
        self.pos = end;
        Ok(v)
    }

    fn read_i64_fixed(&mut self) -> Result<u64> {
        let end = self.pos + 8;
        if end > self.buf.len() {
            return Err(ForgeError::Format("onnx: fixed64 past end".into()));
        }
        let v = u64::from_le_bytes(self.buf[self.pos..end].try_into().unwrap());
        self.pos = end;
        Ok(v)
    }

    /// Skip a field whose value we do not consume.
    fn skip(&mut self, wire: u64) -> Result<()> {
        match wire {
            WIRE_VARINT => {
                self.read_varint()?;
            }
            WIRE_I64 => {
                self.read_i64_fixed()?;
            }
            WIRE_LEN => {
                self.read_len_bytes()?;
            }
            WIRE_I32 => {
                self.read_i32()?;
            }
            other => {
                return Err(ForgeError::Format(format!(
                    "onnx: unknown wire type {other}"
                )));
            }
        }
        Ok(())
    }
}

fn utf8(bytes: &[u8]) -> Result<String> {
    std::str::from_utf8(bytes)
        .map(|s| s.to_string())
        .map_err(|_| ForgeError::Format("onnx: non-UTF8 string field".into()))
}

/// Decode a packed or repeated varint field into i64s.
fn read_packed_varints(bytes: &[u8], out: &mut Vec<i64>) -> Result<()> {
    let mut r = Reader::new(bytes);
    while !r.at_end() {
        out.push(r.read_varint()? as i64);
    }
    Ok(())
}

fn read_packed_f32(bytes: &[u8], out: &mut Vec<f32>) -> Result<()> {
    if bytes.len() % 4 != 0 {
        return Err(ForgeError::Format(
            "onnx: packed float length not /4".into(),
        ));
    }
    for chunk in bytes.chunks_exact(4) {
        out.push(f32::from_le_bytes(chunk.try_into().unwrap()));
    }
    Ok(())
}

fn read_packed_f64(bytes: &[u8], out: &mut Vec<f64>) -> Result<()> {
    if bytes.len() % 8 != 0 {
        return Err(ForgeError::Format(
            "onnx: packed double length not /8".into(),
        ));
    }
    for chunk in bytes.chunks_exact(8) {
        out.push(f64::from_le_bytes(chunk.try_into().unwrap()));
    }
    Ok(())
}

fn read_packed_i32(bytes: &[u8], out: &mut Vec<i32>) -> Result<()> {
    let mut r = Reader::new(bytes);
    while !r.at_end() {
        out.push(r.read_varint()? as i32);
    }
    Ok(())
}

// --- Decoded message structs (the graph IR) ----------------------------------

#[derive(Debug, Default)]
pub struct TensorProto {
    pub dims: Vec<i64>,
    pub data_type: i32,
    pub name: String,
    pub raw_data: Option<Vec<u8>>,
    pub float_data: Vec<f32>,
    pub int32_data: Vec<i32>,
    pub int64_data: Vec<i64>,
    pub double_data: Vec<f64>,
    pub uint64_data: Vec<i64>,
}

#[derive(Debug)]
pub enum AttrValue {
    Float(f32),
    Int(i64),
    String(String),
    Tensor(TensorProto),
    Graph(GraphProto),
    Floats(Vec<f32>),
    Ints(Vec<i64>),
    Strings(Vec<String>),
    Graphs(Vec<GraphProto>),
}

#[derive(Debug)]
pub struct AttributeProto {
    pub name: String,
    pub value: AttrValue,
}

#[derive(Debug, Default)]
pub struct NodeProto {
    pub input: Vec<String>,
    pub output: Vec<String>,
    pub name: String,
    pub op_type: String,
    pub domain: String,
    pub attribute: Vec<AttributeProto>,
}

#[derive(Debug, Default)]
pub struct GraphProto {
    pub node: Vec<NodeProto>,
    pub name: String,
    pub initializer: Vec<TensorProto>,
    pub input: Vec<String>,
    pub output: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ModelProto {
    pub graph: GraphProto,
    pub opset_import: Vec<(String, i64)>,
}

// --- Parsers -----------------------------------------------------------------

fn parse_tensor(bytes: &[u8]) -> Result<TensorProto> {
    let mut t = TensorProto::default();
    let mut r = Reader::new(bytes);
    while !r.at_end() {
        let (field, wire) = r.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => read_packed_varints(r.read_len_bytes()?, &mut t.dims)?,
            (1, WIRE_VARINT) => t.dims.push(r.read_varint()? as i64),
            (2, WIRE_VARINT) => t.data_type = r.read_varint()? as i32,
            (4, WIRE_LEN) => read_packed_f32(r.read_len_bytes()?, &mut t.float_data)?,
            (4, WIRE_I32) => t.float_data.push(f32::from_bits(r.read_i32()?)),
            (5, WIRE_LEN) => read_packed_i32(r.read_len_bytes()?, &mut t.int32_data)?,
            (5, WIRE_VARINT) => t.int32_data.push(r.read_varint()? as i32),
            (7, WIRE_LEN) => read_packed_varints(r.read_len_bytes()?, &mut t.int64_data)?,
            (7, WIRE_VARINT) => t.int64_data.push(r.read_varint()? as i64),
            (8, WIRE_LEN) => t.name = utf8(r.read_len_bytes()?)?,
            (9, WIRE_LEN) => t.raw_data = Some(r.read_len_bytes()?.to_vec()),
            (10, WIRE_LEN) => read_packed_f64(r.read_len_bytes()?, &mut t.double_data)?,
            (11, WIRE_LEN) => read_packed_varints(r.read_len_bytes()?, &mut t.uint64_data)?,
            (_, w) => r.skip(w)?,
        }
    }
    Ok(t)
}

fn parse_attribute(bytes: &[u8]) -> Result<AttributeProto> {
    let mut name = String::new();
    let mut f: Option<f32> = None;
    let mut i: Option<i64> = None;
    let mut s: Option<String> = None;
    let mut tensor: Option<TensorProto> = None;
    let mut graph: Option<GraphProto> = None;
    let mut floats: Vec<f32> = Vec::new();
    let mut ints: Vec<i64> = Vec::new();
    let mut strings: Vec<String> = Vec::new();
    let mut graphs: Vec<GraphProto> = Vec::new();
    let mut attr_type: i64 = 0;

    let mut r = Reader::new(bytes);
    while !r.at_end() {
        let (field, wire) = r.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => name = utf8(r.read_len_bytes()?)?,
            (20, WIRE_VARINT) => attr_type = r.read_varint()? as i64,
            (2, WIRE_I32) => f = Some(f32::from_bits(r.read_i32()?)),
            (3, WIRE_VARINT) => i = Some(r.read_varint()? as i64),
            (4, WIRE_LEN) => s = Some(utf8(r.read_len_bytes()?)?),
            (5, WIRE_LEN) => tensor = Some(parse_tensor(r.read_len_bytes()?)?),
            (6, WIRE_LEN) => graph = Some(parse_graph(r.read_len_bytes()?)?),
            (7, WIRE_LEN) => read_packed_f32(r.read_len_bytes()?, &mut floats)?,
            (7, WIRE_I32) => floats.push(f32::from_bits(r.read_i32()?)),
            (8, WIRE_LEN) => read_packed_varints(r.read_len_bytes()?, &mut ints)?,
            (8, WIRE_VARINT) => ints.push(r.read_varint()? as i64),
            (9, WIRE_LEN) => strings.push(utf8(r.read_len_bytes()?)?),
            (11, WIRE_LEN) => graphs.push(parse_graph(r.read_len_bytes()?)?),
            (_, w) => r.skip(w)?,
        }
    }

    // AttributeType enum: 1 FLOAT 2 INT 3 STRING 4 TENSOR 5 GRAPH 6 FLOATS
    // 7 INTS 8 STRINGS 10 GRAPHS. When `type` is set we trust it; otherwise we
    // pick whichever payload arrived.
    let value = match attr_type {
        1 => AttrValue::Float(f.unwrap_or(0.0)),
        2 => AttrValue::Int(i.unwrap_or(0)),
        3 => AttrValue::String(s.unwrap_or_default()),
        4 => AttrValue::Tensor(tensor.ok_or_else(|| miss(&name, "tensor"))?),
        5 => AttrValue::Graph(graph.ok_or_else(|| miss(&name, "graph"))?),
        6 => AttrValue::Floats(floats),
        7 => AttrValue::Ints(ints),
        8 => AttrValue::Strings(strings),
        10 => AttrValue::Graphs(graphs),
        _ => {
            if let Some(t) = tensor {
                AttrValue::Tensor(t)
            } else if let Some(g) = graph {
                AttrValue::Graph(g)
            } else if let Some(i) = i {
                AttrValue::Int(i)
            } else if let Some(f) = f {
                AttrValue::Float(f)
            } else if let Some(s) = s {
                AttrValue::String(s)
            } else if !ints.is_empty() {
                AttrValue::Ints(ints)
            } else if !floats.is_empty() {
                AttrValue::Floats(floats)
            } else if !graphs.is_empty() {
                AttrValue::Graphs(graphs)
            } else if !strings.is_empty() {
                AttrValue::Strings(strings)
            } else {
                AttrValue::Ints(Vec::new())
            }
        }
    };
    Ok(AttributeProto { name, value })
}

fn miss(attr: &str, kind: &str) -> ForgeError {
    ForgeError::Format(format!("onnx: attribute {attr} declared {kind} but absent"))
}

fn parse_node(bytes: &[u8]) -> Result<NodeProto> {
    let mut n = NodeProto::default();
    let mut r = Reader::new(bytes);
    while !r.at_end() {
        let (field, wire) = r.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => n.input.push(utf8(r.read_len_bytes()?)?),
            (2, WIRE_LEN) => n.output.push(utf8(r.read_len_bytes()?)?),
            (3, WIRE_LEN) => n.name = utf8(r.read_len_bytes()?)?,
            (4, WIRE_LEN) => n.op_type = utf8(r.read_len_bytes()?)?,
            (5, WIRE_LEN) => n.attribute.push(parse_attribute(r.read_len_bytes()?)?),
            (7, WIRE_LEN) => n.domain = utf8(r.read_len_bytes()?)?,
            (_, w) => r.skip(w)?,
        }
    }
    Ok(n)
}

/// ValueInfoProto: we only need the tensor's name (field 1).
fn parse_value_info_name(bytes: &[u8]) -> Result<String> {
    let mut r = Reader::new(bytes);
    while !r.at_end() {
        let (field, wire) = r.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => return utf8(r.read_len_bytes()?),
            (_, w) => r.skip(w)?,
        }
    }
    Ok(String::new())
}

fn parse_graph(bytes: &[u8]) -> Result<GraphProto> {
    let mut g = GraphProto::default();
    let mut r = Reader::new(bytes);
    while !r.at_end() {
        let (field, wire) = r.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => g.node.push(parse_node(r.read_len_bytes()?)?),
            (2, WIRE_LEN) => g.name = utf8(r.read_len_bytes()?)?,
            (5, WIRE_LEN) => g.initializer.push(parse_tensor(r.read_len_bytes()?)?),
            (11, WIRE_LEN) => g.input.push(parse_value_info_name(r.read_len_bytes()?)?),
            (12, WIRE_LEN) => g.output.push(parse_value_info_name(r.read_len_bytes()?)?),
            (_, w) => r.skip(w)?,
        }
    }
    Ok(g)
}

/// OperatorSetIdProto: domain (1, string), version (2, int).
fn parse_opset(bytes: &[u8]) -> Result<(String, i64)> {
    let mut domain = String::new();
    let mut version = 0i64;
    let mut r = Reader::new(bytes);
    while !r.at_end() {
        let (field, wire) = r.read_tag()?;
        match (field, wire) {
            (1, WIRE_LEN) => domain = utf8(r.read_len_bytes()?)?,
            (2, WIRE_VARINT) => version = r.read_varint()? as i64,
            (_, w) => r.skip(w)?,
        }
    }
    Ok((domain, version))
}

/// Parse a full ModelProto from its serialized bytes.
pub fn parse_model(bytes: &[u8]) -> Result<ModelProto> {
    let mut m = ModelProto::default();
    let mut r = Reader::new(bytes);
    while !r.at_end() {
        let (field, wire) = r.read_tag()?;
        match (field, wire) {
            (7, WIRE_LEN) => m.graph = parse_graph(r.read_len_bytes()?)?,
            (8, WIRE_LEN) => m.opset_import.push(parse_opset(r.read_len_bytes()?)?),
            (_, w) => r.skip(w)?,
        }
    }
    Ok(m)
}
