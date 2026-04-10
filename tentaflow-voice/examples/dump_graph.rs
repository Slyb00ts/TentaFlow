// Wypisuje operacje z grafu (i sub-grafow) — kolejnosc + inputs/outputs + atrybuty
use prost::Message;

#[allow(clippy::all)]
mod onnx {
    include!("../src/generated/onnx.rs");
}

fn print_node(node: &onnx::NodeProto, indent: usize) {
    let pad = " ".repeat(indent);
    let inputs: Vec<&str> = node.input.iter().map(|s| s.as_str()).collect();
    let outputs: Vec<&str> = node.output.iter().map(|s| s.as_str()).collect();
    println!(
        "{}[{}] {} ({}):",
        pad,
        node.op_type,
        node.name,
        inputs.len()
    );
    for i in &inputs {
        println!("{}  in:  {}", pad, i);
    }
    for o in &outputs {
        println!("{}  out: {}", pad, o);
    }
    for attr in &node.attribute {
        let val = match attr.r#type {
            1 => format!("f={}", attr.f),
            2 => format!("i={}", attr.i),
            3 => format!("s={}", String::from_utf8_lossy(&attr.s)),
            6 => format!("floats={:?}", attr.floats),
            7 => format!("ints={:?}", attr.ints),
            4 => {
                if let Some(ref t) = attr.t {
                    format!("tensor shape={:?} dtype={}", t.dims, t.data_type)
                } else {
                    "tensor=None".to_string()
                }
            }
            5 => {
                if attr.g.is_some() {
                    "GRAPH".to_string()
                } else {
                    "graph=None".to_string()
                }
            }
            _ => format!("type={}", attr.r#type),
        };
        println!("{}  @{}: {}", pad, attr.name, val);
    }
}

fn walk_graph(graph: &onnx::GraphProto, indent: usize) {
    let pad = " ".repeat(indent);
    println!("{}GRAPH '{}':", pad, graph.name);
    println!("{}  initializers: {}", pad, graph.initializer.len());
    for init in &graph.initializer {
        println!("{}    - {} shape={:?}", pad, init.name, init.dims);
    }
    println!("{}  inputs: {}", pad, graph.input.len());
    for i in &graph.input {
        println!("{}    - {}", pad, i.name);
    }
    println!("{}  outputs: {}", pad, graph.output.len());
    for o in &graph.output {
        println!("{}    - {}", pad, o.name);
    }
    println!("{}  nodes ({}):", pad, graph.node.len());
    for (i, node) in graph.node.iter().enumerate() {
        println!("{}  --- Node {} ---", pad, i);
        print_node(node, indent + 2);
        // Jesli node ma sub-grafy, wejdz w nie
        for attr in &node.attribute {
            if let Some(ref sg) = attr.g {
                println!("{}  >>> Sub-graph '{}' ({}):", pad, attr.name, node.op_type);
                walk_graph(sg, indent + 6);
            }
        }
    }
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: dump_graph <path>");
    let bytes = std::fs::read(&path)?;
    let model = onnx::ModelProto::decode(&*bytes)?;
    let graph = model.graph.unwrap();
    walk_graph(&graph, 0);
    Ok(())
}
