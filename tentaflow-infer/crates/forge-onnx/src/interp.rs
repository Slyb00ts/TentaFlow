// ===== File: interp.rs — hybrid CPU/GPU graph interpreter for the ONNX subset =====
//
// Executes an ONNX graph node by node in topological (file) order. Tensor
// arithmetic (Conv, LSTM, activations, magnitude, reduction) is dispatched to
// GPU Mojo kernels via `Gpu`; shape/control ops (Shape, Gather, Slice, Concat,
// Reshape, If, …) run on the host, exactly as production ONNX runtimes place
// shape logic on the CPU. Subgraphs (`If` branches) execute in a child scope
// that captures outer-scope tensors by name, per the ONNX spec.

use std::collections::HashMap;

use forge_types::{DType, ForgeError, Result};

use crate::gpu::Gpu;
use crate::proto::{AttrValue, GraphProto, ModelProto, NodeProto};
use crate::tensor::{onnx_dtype, Tensor};

/// A lexical scope stack; inner subgraph frames shadow and can read outer ones.
struct Scope {
    frames: Vec<HashMap<String, Tensor>>,
}

impl Scope {
    fn new() -> Self {
        Self { frames: vec![HashMap::new()] }
    }
    fn push(&mut self) {
        self.frames.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.frames.pop();
    }
    fn bind(&mut self, name: &str, t: Tensor) {
        if !name.is_empty() {
            self.frames.last_mut().unwrap().insert(name.to_string(), t);
        }
    }
    fn get(&self, name: &str) -> Result<&Tensor> {
        for frame in self.frames.iter().rev() {
            if let Some(t) = frame.get(name) {
                return Ok(t);
            }
        }
        Err(ForgeError::Format(format!("onnx: value '{name}' not produced")))
    }
    /// Optional input: an empty name means "absent".
    fn get_opt(&self, name: &str) -> Result<Option<Tensor>> {
        if name.is_empty() {
            Ok(None)
        } else {
            Ok(Some(self.get(name)?.clone()))
        }
    }
}

pub struct Session {
    gpu: Gpu,
    model: ModelProto,
}

impl Session {
    pub fn new(gpu: Gpu, model: ModelProto) -> Self {
        Self { gpu, model }
    }

    pub fn model(&self) -> &ModelProto {
        &self.model
    }

    /// Run the model. `inputs` binds graph input names to host tensors; the
    /// returned map holds the graph's declared outputs.
    pub fn run(&self, inputs: HashMap<String, Tensor>) -> Result<HashMap<String, Tensor>> {
        self.gpu.reset()?;
        let mut scope = Scope::new();
        for init in &self.model.graph.initializer {
            scope.bind(&init.name, Tensor::from_proto(init)?);
        }
        for (name, t) in inputs {
            scope.bind(&name, t);
        }
        self.exec_graph(&self.model.graph, &mut scope)?;
        let mut out = HashMap::new();
        for name in &self.model.graph.output {
            out.insert(name.clone(), scope.get(name)?.clone());
        }
        Ok(out)
    }

    fn exec_graph(&self, g: &GraphProto, scope: &mut Scope) -> Result<()> {
        for init in &g.initializer {
            scope.bind(&init.name, Tensor::from_proto(init)?);
        }
        for node in &g.node {
            self.exec_node(node, scope)?;
        }
        Ok(())
    }

    fn exec_node(&self, node: &NodeProto, scope: &mut Scope) -> Result<()> {
        if node.op_type == "If" {
            return self.op_if(node, scope);
        }
        let ins: Vec<Option<Tensor>> = node
            .input
            .iter()
            .map(|n| scope.get_opt(n))
            .collect::<Result<_>>()?;
        let outs = self.dispatch(node, &ins)?;
        if outs.len() != node.output.len() {
            return Err(ForgeError::Format(format!(
                "{} produced {} outputs, node declares {}",
                node.op_type,
                outs.len(),
                node.output.len()
            )));
        }
        for (name, t) in node.output.iter().zip(outs) {
            scope.bind(name, t);
        }
        Ok(())
    }

    fn op_if(&self, node: &NodeProto, scope: &mut Scope) -> Result<()> {
        let cond = scope.get(&node.input[0])?.to_bool_vec()?;
        let take_then = *cond.first().ok_or_else(|| {
            ForgeError::Format("If: condition tensor is empty".into())
        })?;
        let branch_name = if take_then { "then_branch" } else { "else_branch" };
        let branch = attr_graph(node, branch_name)?;
        scope.push();
        let res = (|| {
            self.exec_graph(branch, scope)?;
            branch
                .output
                .iter()
                .map(|n| scope.get(n).cloned())
                .collect::<Result<Vec<_>>>()
        })();
        scope.pop();
        let results = res?;
        for (name, t) in node.output.iter().zip(results) {
            scope.bind(name, t);
        }
        Ok(())
    }

    fn dispatch(&self, node: &NodeProto, ins: &[Option<Tensor>]) -> Result<Vec<Tensor>> {
        let req = |i: usize| -> Result<&Tensor> {
            ins.get(i)
                .and_then(|o| o.as_ref())
                .ok_or_else(|| ForgeError::Format(format!("{}: missing input {i}", node.op_type)))
        };
        let one = |t: Tensor| Ok(vec![t]);
        match node.op_type.as_str() {
            "Constant" => one(op_constant(node)?),
            "ConstantOfShape" => one(op_constant_of_shape(node, req(0)?)?),
            "Identity" => one(req(0)?.clone()),
            "Shape" => one(op_shape(req(0)?)),
            "Size" => one(Tensor::scalar_i64(req(0)?.numel() as i64)),
            "Cast" => one(op_cast(node, req(0)?)?),
            "Gather" => one(op_gather(node, req(0)?, req(1)?)?),
            "Slice" => one(op_slice(req(0)?, req(1)?, req(2)?, ins.get(3).and_then(|o| o.as_ref()), ins.get(4).and_then(|o| o.as_ref()))?),
            "Concat" => one(op_concat(node, ins)?),
            "Unsqueeze" => one(op_unsqueeze(node, req(0)?, ins.get(1).and_then(|o| o.as_ref()))?),
            "Squeeze" => one(op_squeeze(node, req(0)?, ins.get(1).and_then(|o| o.as_ref()))?),
            "Reshape" => one(op_reshape(req(0)?, req(1)?)?),
            "Transpose" => one(op_transpose(node, req(0)?)?),
            "Pad" => one(op_pad(node, req(0)?, req(1)?, ins.get(2).and_then(|o| o.as_ref()))?),
            "Equal" => one(op_equal(req(0)?, req(1)?)?),
            "Not" => one(op_not(req(0)?)?),
            "Relu" => one(self.gpu_unary(req(0)?, |x| self.gpu.relu(x))?),
            "Sigmoid" => one(self.gpu_unary(req(0)?, |x| self.gpu.sigmoid(x))?),
            "Sqrt" => one(self.gpu_unary(req(0)?, |x| self.gpu.sqrt(x))?),
            "Add" => one(self.op_add(req(0)?, req(1)?)?),
            "Pow" => one(self.op_pow(req(0)?, req(1)?)?),
            "Conv" => one(self.op_conv(node, req(0)?, req(1)?, ins.get(2).and_then(|o| o.as_ref()))?),
            "ReduceMean" => one(self.op_reduce_mean(node, req(0)?, ins.get(1).and_then(|o| o.as_ref()))?),
            "LSTM" => self.op_lstm(node, ins),
            other => Err(ForgeError::Unsupported(format!("onnx op '{other}' not implemented"))),
        }
    }

    fn gpu_unary(
        &self,
        x: &Tensor,
        f: impl Fn(&[f32]) -> Result<Vec<f32>>,
    ) -> Result<Tensor> {
        let v = x.to_f32_vec()?;
        Ok(Tensor::from_f32(x.shape.clone(), f(&v)?))
    }

    fn op_add(&self, a: &Tensor, b: &Tensor) -> Result<Tensor> {
        let shape = broadcast_shape(&a.shape, &b.shape)?;
        let av = expand_f32(a, &shape)?;
        let bv = expand_f32(b, &shape)?;
        Ok(Tensor::from_f32(shape, self.gpu.add(&av, &bv)?))
    }

    fn op_pow(&self, base: &Tensor, exp: &Tensor) -> Result<Tensor> {
        let e = exp.to_f32_vec()?;
        let e0 = *e.first().ok_or_else(|| ForgeError::Format("Pow: empty exponent".into()))?;
        if e.iter().any(|&v| v != e0) {
            return Err(ForgeError::Unsupported("Pow: non-uniform exponent".into()));
        }
        let v = base.to_f32_vec()?;
        Ok(Tensor::from_f32(base.shape.clone(), self.gpu.pow(&v, e0)?))
    }

    fn op_conv(
        &self,
        node: &NodeProto,
        x: &Tensor,
        w: &Tensor,
        bias: Option<&Tensor>,
    ) -> Result<Tensor> {
        // x [N=1, in_ch, in_t], w [out_ch, in_ch, ksize]. group=1, dilation=1.
        if x.shape.len() != 3 || x.shape[0] != 1 {
            return Err(ForgeError::Unsupported(format!(
                "Conv: expected x [1, C, T], got {:?}",
                x.shape
            )));
        }
        if attr_int(node, "group").unwrap_or(1) != 1 {
            return Err(ForgeError::Unsupported("Conv: only group=1".into()));
        }
        if attr_ints(node, "dilations").map(|d| d.iter().any(|&x| x != 1)).unwrap_or(false) {
            return Err(ForgeError::Unsupported("Conv: only dilation=1".into()));
        }
        let in_ch = x.shape[1];
        let in_t = x.shape[2];
        let out_ch = w.shape[0];
        if w.shape.len() != 3 || w.shape[1] != in_ch {
            return Err(ForgeError::Format(format!(
                "Conv: weight {:?} incompatible with x {:?}",
                w.shape, x.shape
            )));
        }
        let ksize = w.shape[2];
        let strides = attr_ints(node, "strides").cloned().unwrap_or_default();
        let stride = strides.first().copied().unwrap_or(1).max(1) as usize;
        let pads = attr_ints(node, "pads").cloned().unwrap_or_default();
        let pad = pads.first().copied().unwrap_or(0) as usize;
        let pad_end = pads.get(1).copied().unwrap_or(pad as i64) as usize;
        if pad_end != pad {
            return Err(ForgeError::Unsupported("Conv: asymmetric padding".into()));
        }
        let out_t = (in_t + pad + pad_end).saturating_sub(ksize) / stride + 1;
        let xv = x.to_f32_vec()?;
        let wv = w.to_f32_vec()?;
        let bv = match bias {
            Some(b) => Some(b.to_f32_vec()?),
            None => None,
        };
        let out = self
            .gpu
            .conv1d(&xv, &wv, bv.as_deref(), in_ch, in_t, out_ch, out_t, ksize, stride, pad)?;
        Ok(Tensor::from_f32(vec![1, out_ch, out_t], out))
    }

    fn op_reduce_mean(
        &self,
        node: &NodeProto,
        x: &Tensor,
        axes_in: Option<&Tensor>,
    ) -> Result<Tensor> {
        let rank = x.shape.len();
        let axes_raw = match axes_in {
            Some(t) => t.to_i64_vec()?,
            None => attr_ints(node, "axes").cloned().unwrap_or_default(),
        };
        if axes_raw.len() != 1 {
            return Err(ForgeError::Unsupported(format!(
                "ReduceMean: only a single axis supported, got {axes_raw:?}"
            )));
        }
        let mut axis = axes_raw[0];
        if axis < 0 {
            axis += rank as i64;
        }
        let axis = axis as usize;
        let keepdims = attr_int(node, "keepdims").unwrap_or(1) != 0;
        let outer: usize = x.shape[..axis].iter().product();
        let adim = x.shape[axis];
        let inner: usize = x.shape[axis + 1..].iter().product();
        let v = x.to_f32_vec()?;
        let out = self.gpu.reduce_mean(&v, outer, adim, inner)?;
        let mut shape = x.shape.clone();
        if keepdims {
            shape[axis] = 1;
        } else {
            shape.remove(axis);
        }
        Ok(Tensor::from_f32(shape, out))
    }

    fn op_lstm(&self, node: &NodeProto, ins: &[Option<Tensor>]) -> Result<Vec<Tensor>> {
        let hidden = attr_int(node, "hidden_size")
            .ok_or_else(|| ForgeError::Format("LSTM: missing hidden_size".into()))?
            as usize;
        let x = ins[0].as_ref().ok_or_else(|| ForgeError::Format("LSTM: X missing".into()))?;
        let w = ins[1].as_ref().ok_or_else(|| ForgeError::Format("LSTM: W missing".into()))?;
        let r = ins[2].as_ref().ok_or_else(|| ForgeError::Format("LSTM: R missing".into()))?;
        // X [seq, batch, input]; single direction, batch 1.
        if x.shape.len() != 3 || x.shape[1] != 1 {
            return Err(ForgeError::Unsupported(format!(
                "LSTM: expected X [seq, 1, input], got {:?}",
                x.shape
            )));
        }
        let seq = x.shape[0];
        let input_size = x.shape[2];
        // W [num_dir, 4h, input], R [num_dir, 4h, hidden] — squeeze direction.
        let num_dir = w.shape.first().copied().unwrap_or(1);
        if num_dir != 1 {
            return Err(ForgeError::Unsupported("LSTM: only 1 direction".into()));
        }
        let wv = w.to_f32_vec()?; // [4h*input]
        let rv = r.to_f32_vec()?; // [4h*hidden]
        // B [num_dir, 8h] optional; default zeros.
        let bv = match ins.get(3).and_then(|o| o.as_ref()) {
            Some(b) => b.to_f32_vec()?,
            None => vec![0.0; 8 * hidden],
        };
        // initial_h (idx 5), initial_c (idx 6): [num_dir, batch, hidden].
        let h0 = match ins.get(5).and_then(|o| o.as_ref()) {
            Some(t) => t.to_f32_vec()?,
            None => vec![0.0; hidden],
        };
        let c0 = match ins.get(6).and_then(|o| o.as_ref()) {
            Some(t) => t.to_f32_vec()?,
            None => vec![0.0; hidden],
        };
        let (y, yh, yc) =
            self.gpu.lstm(&x.to_f32_vec()?, &wv, &rv, &bv, &h0, &c0, seq, input_size, hidden)?;
        // Y [seq, num_dir, batch, hidden]; Y_h/Y_c [num_dir, batch, hidden].
        Ok(vec![
            Tensor::from_f32(vec![seq, 1, 1, hidden], y),
            Tensor::from_f32(vec![1, 1, hidden], yh),
            Tensor::from_f32(vec![1, 1, hidden], yc),
        ])
    }
}

// --- Attribute accessors -----------------------------------------------------

fn attr<'a>(node: &'a NodeProto, name: &str) -> Option<&'a AttrValue> {
    node.attribute.iter().find(|a| a.name == name).map(|a| &a.value)
}

fn attr_int(node: &NodeProto, name: &str) -> Option<i64> {
    match attr(node, name)? {
        AttrValue::Int(i) => Some(*i),
        _ => None,
    }
}

fn attr_ints<'a>(node: &'a NodeProto, name: &str) -> Option<&'a Vec<i64>> {
    match attr(node, name)? {
        AttrValue::Ints(v) => Some(v),
        _ => None,
    }
}

fn attr_string<'a>(node: &'a NodeProto, name: &str) -> Option<&'a str> {
    match attr(node, name)? {
        AttrValue::String(s) => Some(s),
        _ => None,
    }
}

fn attr_graph<'a>(node: &'a NodeProto, name: &str) -> Result<&'a GraphProto> {
    match attr(node, name) {
        Some(AttrValue::Graph(g)) => Ok(g),
        _ => Err(ForgeError::Format(format!(
            "{}: missing subgraph attribute {name}",
            node.op_type
        ))),
    }
}

// --- Host op implementations -------------------------------------------------

fn op_constant(node: &NodeProto) -> Result<Tensor> {
    match attr(node, "value") {
        Some(AttrValue::Tensor(t)) => Tensor::from_proto(t),
        _ => match attr(node, "value_int") {
            Some(AttrValue::Int(i)) => Ok(Tensor::scalar_i64(*i)),
            _ => match attr(node, "value_ints") {
                Some(AttrValue::Ints(v)) => Ok(Tensor::from_i64(vec![v.len()], v.clone())),
                _ => match attr(node, "value_float") {
                    Some(AttrValue::Float(f)) => Ok(Tensor::from_f32(vec![], vec![*f])),
                    _ => Err(ForgeError::Unsupported(
                        "Constant: only value / value_int(s) / value_float".into(),
                    )),
                },
            },
        },
    }
}

fn op_constant_of_shape(node: &NodeProto, shape_t: &Tensor) -> Result<Tensor> {
    let shape: Vec<usize> = shape_t.to_i64_vec()?.into_iter().map(|d| d.max(0) as usize).collect();
    let numel: usize = shape.iter().product();
    // `value` is a 1-element tensor giving dtype + fill; default float 0.
    let fill = match attr(node, "value") {
        Some(AttrValue::Tensor(t)) => Tensor::from_proto(t)?,
        _ => Tensor::from_f32(vec![1], vec![0.0]),
    };
    let elem = &fill.data[..fill.dtype.size()];
    let mut data = Vec::with_capacity(numel * fill.dtype.size());
    for _ in 0..numel {
        data.extend_from_slice(elem);
    }
    Ok(Tensor::new(fill.dtype, shape, data))
}

fn op_shape(x: &Tensor) -> Tensor {
    let dims: Vec<i64> = x.shape.iter().map(|&d| d as i64).collect();
    Tensor::from_i64(vec![dims.len()], dims)
}

fn op_cast(node: &NodeProto, x: &Tensor) -> Result<Tensor> {
    let to = attr_int(node, "to").ok_or_else(|| ForgeError::Format("Cast: missing 'to'".into()))?;
    let dt = onnx_dtype(to as i32)?;
    Ok(match dt {
        DType::F32 => Tensor::from_f32(x.shape.clone(), x.to_f32_vec()?),
        DType::F64 => {
            let v: Vec<u8> = x.to_f32_vec()?.iter().flat_map(|&f| (f as f64).to_le_bytes()).collect();
            Tensor::new(DType::F64, x.shape.clone(), v)
        }
        DType::I64 => Tensor::from_i64(x.shape.clone(), x.to_i64_vec()?),
        DType::I32 => {
            let v: Vec<u8> = x.to_i64_vec()?.iter().flat_map(|&i| (i as i32).to_le_bytes()).collect();
            Tensor::new(DType::I32, x.shape.clone(), v)
        }
        DType::Bool => Tensor::from_bool(x.shape.clone(), x.to_bool_vec()?),
        other => {
            return Err(ForgeError::Unsupported(format!("Cast: to dtype {other} unsupported")))
        }
    })
}

/// Row-major strides for a shape.
fn strides(shape: &[usize]) -> Vec<usize> {
    let mut s = vec![1usize; shape.len()];
    for i in (0..shape.len().saturating_sub(1)).rev() {
        s[i] = s[i + 1] * shape[i + 1];
    }
    s
}

fn op_gather(node: &NodeProto, data: &Tensor, indices: &Tensor) -> Result<Tensor> {
    let rank = data.shape.len();
    let mut axis = attr_int(node, "axis").unwrap_or(0);
    if axis < 0 {
        axis += rank as i64;
    }
    let axis = axis as usize;
    if axis >= rank {
        return Err(ForgeError::Format("Gather: axis out of range".into()));
    }
    let esz = data.dtype.size();
    let dstride = strides(&data.shape);
    let axis_dim = data.shape[axis];
    let idx = indices.to_i64_vec()?;
    let idx_norm: Vec<usize> = idx
        .iter()
        .map(|&g| {
            let g = if g < 0 { g + axis_dim as i64 } else { g };
            g.clamp(0, axis_dim as i64 - 1) as usize
        })
        .collect();

    let outer: usize = data.shape[..axis].iter().product();
    let inner: usize = data.shape[axis + 1..].iter().product();
    let inner_bytes = inner * esz;

    let mut out = Vec::with_capacity(outer * idx_norm.len() * inner_bytes);
    for o in 0..outer {
        for &g in &idx_norm {
            let base = (o * axis_dim + g) * dstride[axis];
            let byte = base * esz;
            out.extend_from_slice(&data.data[byte..byte + inner_bytes]);
        }
    }
    let mut shape = Vec::new();
    shape.extend_from_slice(&data.shape[..axis]);
    shape.extend_from_slice(&indices.shape);
    shape.extend_from_slice(&data.shape[axis + 1..]);
    Ok(Tensor::new(data.dtype, shape, out))
}

fn op_slice(
    data: &Tensor,
    starts: &Tensor,
    ends: &Tensor,
    axes: Option<&Tensor>,
    steps: Option<&Tensor>,
) -> Result<Tensor> {
    let rank = data.shape.len();
    let starts = starts.to_i64_vec()?;
    let ends = ends.to_i64_vec()?;
    let axes: Vec<i64> = match axes {
        Some(t) => t.to_i64_vec()?,
        None => (0..starts.len() as i64).collect(),
    };
    let steps: Vec<i64> = match steps {
        Some(t) => t.to_i64_vec()?,
        None => vec![1; starts.len()],
    };
    // Per-axis effective (start, step, count); default = full range, step 1.
    let mut sel: Vec<(i64, i64, usize)> =
        data.shape.iter().map(|&d| (0, 1, d)).collect();
    for i in 0..axes.len() {
        let mut ax = axes[i];
        if ax < 0 {
            ax += rank as i64;
        }
        let ax = ax as usize;
        let dim = data.shape[ax] as i64;
        let step = steps[i];
        if step == 0 {
            return Err(ForgeError::Format("Slice: step 0".into()));
        }
        let mut s = starts[i];
        let mut e = ends[i];
        if s < 0 {
            s += dim;
        }
        if e < 0 {
            e += dim;
        }
        // Clamping differs by step sign (ONNX Slice reference).
        let (s, e) = if step > 0 {
            (s.clamp(0, dim), e.clamp(0, dim))
        } else {
            (s.clamp(0, dim - 1), e.clamp(-1, dim - 1))
        };
        let count = if step > 0 {
            (e - s + step - 1).max(0) / step
        } else {
            (e - s + step + 1).min(0) / step
        };
        sel[ax] = (s, step, count.max(0) as usize);
    }
    let out_shape: Vec<usize> = sel.iter().map(|&(_, _, c)| c).collect();
    let esz = data.dtype.size();
    let dstride = strides(&data.shape);
    let numel: usize = out_shape.iter().product();
    let ostride = strides(&out_shape);
    let mut out = vec![0u8; numel * esz];
    for lin in 0..numel {
        // Decode output multi-index → source byte offset.
        let mut rem = lin;
        let mut src = 0usize;
        for d in 0..rank {
            let coord = rem / ostride[d];
            rem %= ostride[d];
            let (s, st, _c) = sel[d];
            let src_coord = (s + coord as i64 * st) as usize;
            src += src_coord * dstride[d];
        }
        out[lin * esz..lin * esz + esz]
            .copy_from_slice(&data.data[src * esz..src * esz + esz]);
    }
    Ok(Tensor::new(data.dtype, out_shape, out))
}

fn op_concat(node: &NodeProto, ins: &[Option<Tensor>]) -> Result<Tensor> {
    let tensors: Vec<&Tensor> = ins.iter().filter_map(|o| o.as_ref()).collect();
    if tensors.is_empty() {
        return Err(ForgeError::Format("Concat: no inputs".into()));
    }
    let rank = tensors[0].shape.len();
    let mut axis = attr_int(node, "axis").unwrap_or(0);
    if axis < 0 {
        axis += rank as i64;
    }
    let axis = axis as usize;
    let dtype = tensors[0].dtype;
    let esz = dtype.size();
    // Output shape: sum along axis, others identical.
    let mut out_shape = tensors[0].shape.clone();
    out_shape[axis] = tensors.iter().map(|t| t.shape[axis]).sum();
    let outer: usize = out_shape[..axis].iter().product();
    let inner_bytes: usize = out_shape[axis + 1..].iter().product::<usize>() * esz;
    let mut out = Vec::with_capacity(out_shape.iter().product::<usize>() * esz);
    for o in 0..outer {
        for t in &tensors {
            let ad = t.shape[axis];
            let seg = ad * inner_bytes;
            let start = o * seg;
            out.extend_from_slice(&t.data[start..start + seg]);
        }
    }
    Ok(Tensor::new(dtype, out_shape, out))
}

/// Resolve an axes list (from a 2nd input or an attribute) to positive indices.
fn resolve_axes(node: &NodeProto, axes_in: Option<&Tensor>, rank: i64) -> Result<Vec<i64>> {
    let raw = match axes_in {
        Some(t) => t.to_i64_vec()?,
        None => attr_ints(node, "axes").cloned().unwrap_or_default(),
    };
    Ok(raw.into_iter().map(|a| if a < 0 { a + rank } else { a }).collect())
}

fn op_unsqueeze(node: &NodeProto, x: &Tensor, axes_in: Option<&Tensor>) -> Result<Tensor> {
    let out_rank = x.shape.len() as i64 + count_axes(node, axes_in)? as i64;
    let axes = resolve_axes(node, axes_in, out_rank)?;
    let mut shape = x.shape.clone();
    let mut sorted = axes.clone();
    sorted.sort_unstable();
    for a in sorted {
        let a = a as usize;
        if a > shape.len() {
            return Err(ForgeError::Format("Unsqueeze: axis out of range".into()));
        }
        shape.insert(a, 1);
    }
    Ok(Tensor::new(x.dtype, shape, x.data.clone()))
}

fn count_axes(node: &NodeProto, axes_in: Option<&Tensor>) -> Result<usize> {
    Ok(match axes_in {
        Some(t) => t.numel(),
        None => attr_ints(node, "axes").map(|v| v.len()).unwrap_or(0),
    })
}

fn op_squeeze(node: &NodeProto, x: &Tensor, axes_in: Option<&Tensor>) -> Result<Tensor> {
    let rank = x.shape.len() as i64;
    let axes = resolve_axes(node, axes_in, rank)?;
    let shape: Vec<usize> = if axes.is_empty() {
        x.shape.iter().copied().filter(|&d| d != 1).collect()
    } else {
        let drop: Vec<usize> = axes.iter().map(|&a| a as usize).collect();
        x.shape
            .iter()
            .enumerate()
            .filter(|(i, _)| !drop.contains(i))
            .map(|(_, &d)| d)
            .collect()
    };
    Ok(Tensor::new(x.dtype, shape, x.data.clone()))
}

fn op_reshape(x: &Tensor, shape_t: &Tensor) -> Result<Tensor> {
    let req = shape_t.to_i64_vec()?;
    let numel = x.numel();
    let mut shape: Vec<usize> = Vec::with_capacity(req.len());
    let mut neg = None;
    for (i, &d) in req.iter().enumerate() {
        match d {
            -1 => {
                neg = Some(i);
                shape.push(1); // placeholder
            }
            0 => shape.push(x.shape.get(i).copied().unwrap_or(1)),
            d if d >= 0 => shape.push(d as usize),
            _ => return Err(ForgeError::Format("Reshape: invalid dim".into())),
        }
    }
    if let Some(i) = neg {
        let known: usize = shape.iter().enumerate().filter(|(j, _)| *j != i).map(|(_, &d)| d).product();
        shape[i] = numel.checked_div(known).unwrap_or(0);
    }
    if shape.iter().product::<usize>() != numel {
        return Err(ForgeError::Format(format!(
            "Reshape: {:?} does not match {} elements",
            shape, numel
        )));
    }
    Ok(Tensor::new(x.dtype, shape, x.data.clone()))
}

fn op_transpose(node: &NodeProto, x: &Tensor) -> Result<Tensor> {
    let rank = x.shape.len();
    let perm: Vec<usize> = match attr_ints(node, "perm") {
        Some(p) => p.iter().map(|&a| a as usize).collect(),
        None => (0..rank).rev().collect(),
    };
    if perm.len() != rank {
        return Err(ForgeError::Format("Transpose: perm rank mismatch".into()));
    }
    let out_shape: Vec<usize> = perm.iter().map(|&p| x.shape[p]).collect();
    let esz = x.dtype.size();
    let in_stride = strides(&x.shape);
    let out_stride = strides(&out_shape);
    let numel = x.numel();
    let mut out = vec![0u8; numel * esz];
    for lin in 0..numel {
        let mut rem = lin;
        let mut src = 0usize;
        for d in 0..rank {
            let coord = rem / out_stride[d];
            rem %= out_stride[d];
            src += coord * in_stride[perm[d]];
        }
        out[lin * esz..lin * esz + esz]
            .copy_from_slice(&x.data[src * esz..src * esz + esz]);
    }
    Ok(Tensor::new(x.dtype, out_shape, out))
}

fn op_pad(
    node: &NodeProto,
    x: &Tensor,
    pads_t: &Tensor,
    const_val: Option<&Tensor>,
) -> Result<Tensor> {
    let rank = x.shape.len();
    let pads = pads_t.to_i64_vec()?;
    if pads.len() != 2 * rank {
        return Err(ForgeError::Format("Pad: pads length must be 2*rank".into()));
    }
    let mode = attr_string(node, "mode").unwrap_or("constant");
    let cval = match const_val {
        Some(t) => *t.to_f32_vec()?.first().unwrap_or(&0.0),
        None => 0.0,
    };
    let begin: Vec<i64> = pads[..rank].to_vec();
    let end: Vec<i64> = pads[rank..].to_vec();
    let out_shape: Vec<usize> = (0..rank)
        .map(|d| (x.shape[d] as i64 + begin[d] + end[d]).max(0) as usize)
        .collect();
    let xv = x.to_f32_vec()?;
    let in_stride = strides(&x.shape);
    let out_stride = strides(&out_shape);
    let numel: usize = out_shape.iter().product();
    let mut out = vec![0.0f32; numel];
    for (lin, slot) in out.iter_mut().enumerate() {
        let mut src = 0usize;
        let mut oob = false;
        for d in 0..rank {
            let coord = lin / out_stride[d] % out_shape[d];
            let dim = x.shape[d] as i64;
            let mut ic = coord as i64 - begin[d];
            match mode {
                "reflect" => {
                    // Reflect without repeating the edge (numpy 'reflect').
                    if dim > 1 {
                        let period = 2 * (dim - 1);
                        ic = ((ic % period) + period) % period;
                        if ic >= dim {
                            ic = period - ic;
                        }
                    } else {
                        ic = 0;
                    }
                }
                "edge" => {
                    ic = ic.clamp(0, dim - 1);
                }
                _ => {
                    if ic < 0 || ic >= dim {
                        oob = true;
                    }
                }
            }
            if !oob {
                src += ic as usize * in_stride[d];
            }
        }
        *slot = if oob { cval } else { xv[src] };
    }
    Ok(Tensor::from_f32(out_shape, out))
}

fn op_equal(a: &Tensor, b: &Tensor) -> Result<Tensor> {
    let shape = broadcast_shape(&a.shape, &b.shape)?;
    let av = expand_i64(a, &shape)?;
    let bv = expand_i64(b, &shape)?;
    let out: Vec<bool> = av.iter().zip(&bv).map(|(x, y)| x == y).collect();
    Ok(Tensor::from_bool(shape, out))
}

fn op_not(x: &Tensor) -> Result<Tensor> {
    Ok(Tensor::from_bool(x.shape.clone(), x.to_bool_vec()?.into_iter().map(|b| !b).collect()))
}

// --- Broadcasting ------------------------------------------------------------

fn broadcast_shape(a: &[usize], b: &[usize]) -> Result<Vec<usize>> {
    let rank = a.len().max(b.len());
    let mut out = vec![0usize; rank];
    for i in 0..rank {
        let da = if i < rank - a.len() { 1 } else { a[i - (rank - a.len())] };
        let db = if i < rank - b.len() { 1 } else { b[i - (rank - b.len())] };
        out[i] = if da == db {
            da
        } else if da == 1 {
            db
        } else if db == 1 {
            da
        } else {
            return Err(ForgeError::Format(format!(
                "broadcast: incompatible dims {da} vs {db}"
            )));
        };
    }
    Ok(out)
}

/// Broadcast a tensor's f32 values to `target` shape (row-major, contiguous).
fn expand_f32(t: &Tensor, target: &[usize]) -> Result<Vec<f32>> {
    let v = t.to_f32_vec()?;
    Ok(expand_generic(&v, &t.shape, target))
}

fn expand_i64(t: &Tensor, target: &[usize]) -> Result<Vec<i64>> {
    let v = t.to_i64_vec()?;
    Ok(expand_generic(&v, &t.shape, target))
}

// `d` indexes several stride/shape arrays at once; enumerate on one obscures it.
#[allow(clippy::needless_range_loop)]
fn expand_generic<T: Copy>(v: &[T], shape: &[usize], target: &[usize]) -> Vec<T> {
    if shape == target {
        return v.to_vec();
    }
    let rank = target.len();
    let off = rank - shape.len();
    let in_stride = strides(shape);
    let out_stride = strides(target);
    let numel: usize = target.iter().product();
    let mut out = Vec::with_capacity(numel);
    for lin in 0..numel {
        let mut rem = lin;
        let mut src = 0usize;
        for d in 0..rank {
            let coord = rem / out_stride[d];
            rem %= out_stride[d];
            if d >= off {
                let sd = d - off;
                let c = if shape[sd] == 1 { 0 } else { coord };
                src += c * in_stride[sd];
            }
        }
        out.push(v[src]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::AttributeProto;

    #[test]
    fn gather_axis0_scalar_index() {
        // data [2,1,128]-like reduced to [2,3]; gather row 1.
        let data = Tensor::from_f32(vec![2, 3], vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
        let node = NodeProto {
            op_type: "Gather".into(),
            attribute: vec![AttributeProto { name: "axis".into(), value: AttrValue::Int(0) }],
            ..Default::default()
        };
        let out = op_gather(&node, &data, &Tensor::scalar_i64(1)).unwrap();
        assert_eq!(out.shape, vec![3]);
        assert_eq!(out.to_f32_vec().unwrap(), vec![3.0, 4.0, 5.0]);
    }

    #[test]
    fn slice_negative_step_reverses() {
        let data = Tensor::from_i64(vec![4], vec![10, 20, 30, 40]);
        // start=3, end=-5 (→ before 0), step=-1 → full reverse.
        let out = op_slice(
            &data,
            &Tensor::from_i64(vec![1], vec![3]),
            &Tensor::from_i64(vec![1], vec![-5]),
            Some(&Tensor::from_i64(vec![1], vec![0])),
            Some(&Tensor::from_i64(vec![1], vec![-1])),
        )
        .unwrap();
        assert_eq!(out.to_i64_vec().unwrap(), vec![40, 30, 20, 10]);
    }

    #[test]
    fn transpose_2d() {
        let x = Tensor::from_f32(vec![2, 3], vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        let node = NodeProto {
            op_type: "Transpose".into(),
            attribute: vec![AttributeProto {
                name: "perm".into(),
                value: AttrValue::Ints(vec![1, 0]),
            }],
            ..Default::default()
        };
        let out = op_transpose(&node, &x).unwrap();
        assert_eq!(out.shape, vec![3, 2]);
        assert_eq!(out.to_f32_vec().unwrap(), vec![1.0, 4.0, 2.0, 5.0, 3.0, 6.0]);
    }

    #[test]
    fn reshape_infers_negative_one() {
        let x = Tensor::from_f32(vec![2, 3], vec![0.0; 6]);
        let out = op_reshape(&x, &Tensor::from_i64(vec![2], vec![-1, 2])).unwrap();
        assert_eq!(out.shape, vec![3, 2]);
    }

    #[test]
    fn concat_axis0() {
        let a = Tensor::from_i64(vec![1, 2], vec![1, 2]);
        let b = Tensor::from_i64(vec![2, 2], vec![3, 4, 5, 6]);
        let node = NodeProto {
            op_type: "Concat".into(),
            attribute: vec![AttributeProto { name: "axis".into(), value: AttrValue::Int(0) }],
            ..Default::default()
        };
        let out = op_concat(&node, &[Some(a), Some(b)]).unwrap();
        assert_eq!(out.shape, vec![3, 2]);
        assert_eq!(out.to_i64_vec().unwrap(), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn pad_reflect_last_axis() {
        // [1,4] reflect-pad by 2 on each side of the last axis (numpy 'reflect').
        let x = Tensor::from_f32(vec![1, 4], vec![1.0, 2.0, 3.0, 4.0]);
        let node = NodeProto {
            op_type: "Pad".into(),
            attribute: vec![AttributeProto {
                name: "mode".into(),
                value: AttrValue::String("reflect".into()),
            }],
            ..Default::default()
        };
        let pads = Tensor::from_i64(vec![4], vec![0, 2, 0, 2]);
        let out = op_pad(&node, &x, &pads, None).unwrap();
        assert_eq!(out.shape, vec![1, 8]);
        assert_eq!(
            out.to_f32_vec().unwrap(),
            vec![3.0, 2.0, 1.0, 2.0, 3.0, 4.0, 3.0, 2.0]
        );
    }

    #[test]
    fn equal_broadcasts() {
        let a = Tensor::from_i64(vec![3], vec![16000, 8000, 16000]);
        let b = Tensor::scalar_i64(16000);
        let out = op_equal(&a, &b).unwrap();
        assert_eq!(out.to_bool_vec().unwrap(), vec![true, false, true]);
    }
}
