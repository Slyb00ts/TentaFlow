// Wypisuje wartosci int64 Constant w then_branch (do parametrow Slice)
use prost::Message;

#[allow(clippy::all)]
mod onnx {
    include!("../src/generated/onnx.rs");
}

fn dump_constants(graph: &onnx::GraphProto) {
    for node in &graph.node {
        if node.op_type == "Constant" {
            let out = node.output.first().map(|s| s.as_str()).unwrap_or("?");
            for attr in &node.attribute {
                if attr.name == "value" {
                    if let Some(ref t) = attr.t {
                        let dtype = t.data_type;
                        // Pokaz tylko int64 (7) i f32 (1)
                        let val_str = if dtype == 7 {
                            // int64 — w raw_data albo int64_data
                            if !t.int64_data.is_empty() {
                                format!("{:?}", t.int64_data)
                            } else if !t.raw_data.is_empty() {
                                let mut vals = Vec::new();
                                for c in t.raw_data.chunks_exact(8) {
                                    vals.push(i64::from_le_bytes([
                                        c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7],
                                    ]));
                                }
                                format!("{:?}", vals)
                            } else {
                                "EMPTY".to_string()
                            }
                        } else if dtype == 1 && t.dims.iter().product::<i64>() < 5 {
                            // f32 male
                            if !t.float_data.is_empty() {
                                format!("{:?}", t.float_data)
                            } else if !t.raw_data.is_empty() {
                                let mut vals = Vec::new();
                                for c in t.raw_data.chunks_exact(4) {
                                    vals.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
                                }
                                format!("{:?}", vals)
                            } else {
                                continue;
                            }
                        } else {
                            continue;
                        };
                        if out.contains("stft") || out.contains("Constant_") {
                            println!("  {} shape={:?} dtype={} = {}", out, t.dims, dtype, val_str);
                        }
                    }
                }
            }
        }
        // Recurse subgraphs
        for attr in &node.attribute {
            if let Some(ref sg) = attr.g {
                dump_constants(sg);
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("path");
    let bytes = std::fs::read(&path)?;
    let model = onnx::ModelProto::decode(&*bytes)?;
    let graph = model.graph.unwrap();
    dump_constants(&graph);
    Ok(())
}
