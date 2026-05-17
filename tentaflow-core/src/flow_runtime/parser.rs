// =============================================================================
// File: flow_runtime/parser.rs — load + validate + topo-sort *.flow.json
// =============================================================================
//
// Three entry points:
//   * `parse_flow_definition` — JSON -> `FlowDefinition` (serde only).
//   * `compile`               — `FlowDefinition` -> `CompiledFlow` after
//                               schema, edge, port, and cycle checks.
//   * `load_from_addon_dir`   — read a file from an addon bundle via
//                               `util::path_safety::safe_resolve`, then
//                               parse+compile in one call.
//
// Cycle detection uses iterative 3-color DFS; when a gray->gray back edge
// fires the function reconstructs the cycle from the parent map so the
// error message identifies the offending nodes (not just "cycle found").
// The topological order is produced by Kahn's algorithm over the same
// adjacency map so callers receive a single consistent traversal.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use super::types::{
    CompiledFlow, EdgeDef, FlowCompileError, FlowDefinition, OperatorType, BRANCH_PORTS,
    MAX_OPERATORS_PER_FLOW, SUPPORTED_SCHEMA_VERSION,
};

/// Parses a `*.flow.json` document. Returns `Parse` for any serde failure.
pub fn parse_flow_definition(json: &str) -> Result<FlowDefinition, FlowCompileError> {
    serde_json::from_str::<FlowDefinition>(json).map_err(|e| FlowCompileError::Parse(e.to_string()))
}

/// Validates the document and produces a `CompiledFlow`. Fails fast on the
/// first structural problem — the install path bubbles the error up.
pub fn compile(def: FlowDefinition) -> Result<CompiledFlow, FlowCompileError> {
    if def.schema_version != SUPPORTED_SCHEMA_VERSION {
        return Err(FlowCompileError::UnsupportedSchemaVersion {
            found: def.schema_version,
        });
    }
    if def.operators.is_empty() {
        return Err(FlowCompileError::EmptyFlow);
    }
    if def.operators.len() > MAX_OPERATORS_PER_FLOW {
        return Err(FlowCompileError::TooManyOperators {
            count: def.operators.len(),
        });
    }

    // Operator id -> type, also detects duplicates.
    let mut op_types: HashMap<String, OperatorType> = HashMap::with_capacity(def.operators.len());
    for op in &def.operators {
        if op_types.insert(op.id.clone(), op.op_type).is_some() {
            return Err(FlowCompileError::DuplicateOperator(op.id.clone()));
        }
    }

    validate_edges(&def.edges, &op_types)?;

    let adjacency = build_adjacency(&def.operators, &def.edges);
    detect_cycle(&adjacency)?;
    let topo_order = topo_sort(&adjacency)?;

    Ok(CompiledFlow {
        def,
        topo_order,
        adjacency,
    })
}

/// Reads `<addon_dir>/<relative_path>`, parses it, compiles it. The path is
/// resolved through `util::path_safety::safe_resolve` so traversal /
/// symlink / absolute-path inputs are rejected before any I/O.
pub fn load_from_addon_dir(
    addon_dir: &Path,
    relative_path: &str,
) -> Result<CompiledFlow, FlowCompileError> {
    let resolved = crate::util::path_safety::safe_resolve(addon_dir, relative_path)
        .map_err(|e| FlowCompileError::Path(e.to_string()))?;
    let bytes = std::fs::read_to_string(&resolved).map_err(|e| FlowCompileError::Io(e.to_string()))?;
    let def = parse_flow_definition(&bytes)?;
    compile(def)
}

// -- internals --------------------------------------------------------------

fn validate_edges(
    edges: &[EdgeDef],
    op_types: &HashMap<String, OperatorType>,
) -> Result<(), FlowCompileError> {
    for (idx, edge) in edges.iter().enumerate() {
        let from_type = op_types.get(&edge.from).ok_or_else(|| {
            FlowCompileError::EdgeReferencesUnknownOperator {
                edge_idx: idx,
                op_id: edge.from.clone(),
            }
        })?;
        if !op_types.contains_key(&edge.to) {
            return Err(FlowCompileError::EdgeReferencesUnknownOperator {
                edge_idx: idx,
                op_id: edge.to.clone(),
            });
        }
        if let Some(port) = edge.port.as_deref() {
            if *from_type != OperatorType::Branch {
                return Err(FlowCompileError::PortOnNonBranch {
                    edge_idx: idx,
                    op_id: edge.from.clone(),
                    port: port.to_string(),
                });
            }
            if !BRANCH_PORTS.contains(&port) {
                return Err(FlowCompileError::InvalidPort {
                    edge_idx: idx,
                    port: port.to_string(),
                });
            }
        }
    }
    Ok(())
}

fn build_adjacency(
    operators: &[super::types::OperatorDef],
    edges: &[EdgeDef],
) -> HashMap<String, Vec<String>> {
    let mut adj: HashMap<String, Vec<String>> = HashMap::with_capacity(operators.len());
    for op in operators {
        adj.entry(op.id.clone()).or_default();
    }
    for edge in edges {
        adj.entry(edge.from.clone())
            .or_default()
            .push(edge.to.clone());
    }
    adj
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Color {
    White,
    Gray,
    Black,
}

/// Iterative 3-color DFS. On a gray->gray back edge the cycle is rebuilt
/// from the discovery-time parent map so the diagnostic lists the actual
/// nodes involved.
fn detect_cycle(adj: &HashMap<String, Vec<String>>) -> Result<(), FlowCompileError> {
    let mut color: HashMap<&str, Color> = adj.keys().map(|k| (k.as_str(), Color::White)).collect();
    let mut parent: HashMap<&str, Option<&str>> = HashMap::new();

    // Deterministic iteration so the same input always reports the same
    // cycle. HashMap key order varies between runs, so collect+sort first.
    let mut roots: Vec<&str> = adj.keys().map(|k| k.as_str()).collect();
    roots.sort_unstable();

    for root in roots {
        if color[root] != Color::White {
            continue;
        }
        // Stack frame: (node, index into adj[node] of next neighbor to visit).
        let mut stack: Vec<(&str, usize)> = vec![(root, 0)];
        color.insert(root, Color::Gray);
        parent.insert(root, None);

        while let Some(&mut (node, ref mut next_idx)) = stack.last_mut() {
            let neighbors = &adj[node];
            if *next_idx >= neighbors.len() {
                color.insert(node, Color::Black);
                stack.pop();
                continue;
            }
            let neighbor = neighbors[*next_idx].as_str();
            *next_idx += 1;

            match color[neighbor] {
                Color::White => {
                    color.insert(neighbor, Color::Gray);
                    parent.insert(neighbor, Some(node));
                    stack.push((neighbor, 0));
                }
                Color::Gray => {
                    return Err(FlowCompileError::Cycle {
                        involved: reconstruct_cycle(node, neighbor, &parent),
                    });
                }
                Color::Black => {}
            }
        }
    }

    Ok(())
}

/// Walks the parent map back from `from_node` until it hits `back_target`,
/// then closes the cycle. Returns the nodes in traversal order.
fn reconstruct_cycle(
    from_node: &str,
    back_target: &str,
    parent: &HashMap<&str, Option<&str>>,
) -> Vec<String> {
    let mut path = vec![from_node.to_string()];
    let mut cursor = from_node;
    while cursor != back_target {
        match parent.get(cursor).and_then(|p| *p) {
            Some(p) => {
                path.push(p.to_string());
                cursor = p;
            }
            None => break,
        }
    }
    path.reverse();
    path.push(back_target.to_string());
    path
}

/// Kahn's algorithm. Cycles were already rejected; remaining nodes form a
/// DAG so the algorithm always drains every operator. Deterministic order
/// (lexicographic tiebreak) keeps test output stable.
fn topo_sort(adj: &HashMap<String, Vec<String>>) -> Result<Vec<String>, FlowCompileError> {
    let mut in_degree: HashMap<&str, usize> = adj.keys().map(|k| (k.as_str(), 0usize)).collect();
    for outs in adj.values() {
        for to in outs {
            *in_degree.entry(to.as_str()).or_insert(0) += 1;
        }
    }

    let mut ready: Vec<&str> = in_degree
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(k, _)| *k)
        .collect();
    ready.sort_unstable();
    let mut queue: VecDeque<&str> = ready.into_iter().collect();

    let mut out: Vec<String> = Vec::with_capacity(adj.len());
    let mut seen: HashSet<&str> = HashSet::new();
    while let Some(node) = queue.pop_front() {
        if !seen.insert(node) {
            continue;
        }
        out.push(node.to_string());
        let mut next: Vec<&str> = adj[node].iter().map(|s| s.as_str()).collect();
        next.sort_unstable();
        for n in next {
            let entry = in_degree.entry(n).or_insert(0);
            if *entry > 0 {
                *entry -= 1;
            }
            if *entry == 0 && !seen.contains(n) {
                queue.push_back(n);
            }
        }
    }

    if out.len() != adj.len() {
        // Should not happen — cycle detection precedes topo. Defensive.
        return Err(FlowCompileError::Cycle {
            involved: adj
                .keys()
                .filter(|k| !out.contains(k))
                .cloned()
                .collect(),
        });
    }
    Ok(out)
}
