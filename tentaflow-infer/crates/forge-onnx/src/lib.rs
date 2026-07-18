// ===== File: lib.rs — forge-onnx: ONNX subset loader + GPU executor =====
//
// A real ONNX importer (SPEC §4.1, "ONNX subset, opset 17+"): parses the graph
// protobuf into a small typed IR (nodes, edges, initializers, subgraphs) and
// runs it with a hybrid CPU/GPU interpreter. Heavy tensor arithmetic executes
// as Mojo f32 GPU kernels (forge-kernels); shape/control ops run on the host.
// The concrete validated target is Silero VAD (`silero_vad.onnx`).

mod gpu;
mod interp;
mod proto;
mod tensor;

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use forge_hal::Device;
use forge_types::Result;

pub use interp::Session;
pub use proto::{GraphProto, ModelProto, NodeProto};
pub use tensor::Tensor;

/// Parse an ONNX model file into its graph IR (no device required).
pub fn load_model(path: impl AsRef<Path>) -> Result<ModelProto> {
    let bytes = std::fs::read(path.as_ref())?;
    proto::parse_model(&bytes)
}

/// Every distinct op type in the graph and its subgraphs, with counts. Useful
/// for reporting coverage of a model before running it.
pub fn op_histogram(model: &ModelProto) -> BTreeMap<String, usize> {
    let mut hist = BTreeMap::new();
    fn walk(g: &GraphProto, hist: &mut BTreeMap<String, usize>) {
        for n in &g.node {
            *hist.entry(n.op_type.clone()).or_insert(0) += 1;
            for a in &n.attribute {
                match &a.value {
                    proto::AttrValue::Graph(sg) => walk(sg, hist),
                    proto::AttrValue::Graphs(sgs) => {
                        for sg in sgs {
                            walk(sg, hist);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    walk(&model.graph, &mut hist);
    hist
}

/// Load a model and bind it to a device, ready to run.
pub fn load_session(device: Arc<dyn Device>, path: impl AsRef<Path>) -> Result<Session> {
    let model = load_model(path)?;
    let gpu = gpu::Gpu::new(device)?;
    Ok(Session::new(gpu, model))
}

/// Convenience: run a model with the given named inputs on the device.
pub fn run(
    device: Arc<dyn Device>,
    path: impl AsRef<Path>,
    inputs: HashMap<String, Tensor>,
) -> Result<HashMap<String, Tensor>> {
    load_session(device, path)?.run(inputs)
}
