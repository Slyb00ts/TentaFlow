// Deep inspect — zaglada do struktury ONNX bardziej niz tylko initializers
use prost::Message;
use std::env;
use std::fs;

#[allow(clippy::all)]
mod onnx {
    include!("../src/generated/onnx.rs");
}

fn main() -> anyhow::Result<()> {
    let path = env::args().nth(1).expect("path");
    let bytes = fs::read(&path)?;
    let model = onnx::ModelProto::decode(&*bytes)?;

    println!("IR version: {}", model.ir_version);
    println!("Producer: {}", model.producer_name);
    println!("Opset imports:");
    for op in &model.opset_import {
        println!("  {} v{}", op.domain, op.version);
    }

    let graph = model.graph.as_ref().unwrap();
    println!("\nGraph name: {}", graph.name);
    println!("Inputs: {}", graph.input.len());
    for i in &graph.input {
        println!("  - {}", i.name);
    }
    println!("Outputs: {}", graph.output.len());
    for o in &graph.output {
        println!("  - {}", o.name);
    }
    println!("Initializers: {}", graph.initializer.len());
    println!("Sparse initializers: {}", graph.sparse_initializer.len());
    println!("Nodes: {}", graph.node.len());

    let mut op_counts = std::collections::HashMap::new();
    for n in &graph.node {
        *op_counts.entry(n.op_type.clone()).or_insert(0) += 1;
    }
    println!("\nOp counts:");
    let mut sorted: Vec<_> = op_counts.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));
    for (op, cnt) in sorted {
        println!("  {}: {}", op, cnt);
    }

    // Pokaz pierwsze 5 Constant node z peka nazw i shape
    println!("\nFirst 15 Constant nodes:");
    let mut shown = 0;
    for n in &graph.node {
        if n.op_type != "Constant" {
            continue;
        }
        shown += 1;
        if shown > 15 {
            break;
        }
        let out_name = n.output.first().map(|s| s.as_str()).unwrap_or("?");
        let mut info = format!("  [{}] output={}", n.name, out_name);
        for a in &n.attribute {
            if a.name == "value" {
                if let Some(ref t) = a.t {
                    info.push_str(&format!(" | value shape={:?} data_type={} raw_len={} float_len={}",
                        t.dims, t.data_type, t.raw_data.len(), t.float_data.len()));
                }
            } else {
                info.push_str(&format!(" | attr={}", a.name));
            }
        }
        println!("{}", info);
    }

    println!("\nFunctions: {}", model.functions.len());
    for f in &model.functions {
        println!("  - {} (nodes: {})", f.name, f.node.len());
    }
    Ok(())
}
