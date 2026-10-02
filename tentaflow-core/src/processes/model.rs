// ============ File: model.rs — B1 process graph validation and structured parallel joins ============

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{bail, ensure, Context, Result};
use tentaflow_protocol::processes::{ProcessModel, ProcessNodeKind};

use crate::flow_engine::expr;

pub const MAX_MODEL_BYTES: usize = 512 * 1024;
pub const MAX_NODES: usize = 128;
pub const MAX_SEQUENCE_FLOWS: usize = 256;
pub const MAX_VARIABLE_BYTES: usize = 256 * 1024;
pub const MAX_VARIABLE_KEYS: usize = 128;
const MAX_DI_COORDINATE: f64 = 1_000_000.0;

pub fn starter_model() -> ProcessModel {
    use tentaflow_protocol::processes::{ProcessDiagram, ProcessNode, ProcessSequenceFlow};
    ProcessModel {
        schema_version: 1,
        process_id: "Process_1".into(),
        nodes: vec![
            ProcessNode {
                id: "Start_1".into(),
                name: "Start".into(),
                kind: ProcessNodeKind::Start,
            },
            ProcessNode {
                id: "End_1".into(),
                name: "End".into(),
                kind: ProcessNodeKind::End,
            },
        ],
        sequence_flows: vec![ProcessSequenceFlow {
            id: "Flow_1".into(),
            source_id: "Start_1".into(),
            target_id: "End_1".into(),
            condition: None,
        }],
        variables: Default::default(),
        diagram: ProcessDiagram::default(),
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.as_bytes()[0].is_ascii_alphabetic()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

pub fn validate_variables(value: &serde_json::Value) -> Result<()> {
    let object = value
        .as_object()
        .context("process variables must be an object")?;
    ensure!(
        object.len() <= MAX_VARIABLE_KEYS,
        "too many process variables"
    );
    ensure!(
        serde_json::to_vec(value)?.len() <= MAX_VARIABLE_BYTES,
        "process variables exceed 256 KiB"
    );
    ensure!(
        object.keys().all(|key| valid_id(key)),
        "invalid process variable name"
    );
    Ok(())
}

pub fn validate_draft(model: &ProcessModel) -> Result<()> {
    ensure!(
        model.schema_version == 1 && valid_id(&model.process_id),
        "invalid process model identity"
    );
    ensure!(
        model.nodes.len() <= MAX_NODES && model.sequence_flows.len() <= MAX_SEQUENCE_FLOWS,
        "process draft exceeds B1 graph limits"
    );
    ensure!(
        serde_json::to_vec(model)?.len() <= MAX_MODEL_BYTES,
        "process draft exceeds 512 KiB"
    );
    validate_variables(&serde_json::to_value(&model.variables)?)?;
    let mut node_ids = HashSet::new();
    for node in &model.nodes {
        ensure!(
            valid_id(&node.id) && node_ids.insert(node.id.as_str()),
            "invalid or duplicate node ID"
        );
        ensure!(
            node.name.len() <= 256 && !node.name.chars().any(char::is_control),
            "invalid node name"
        );
        match &node.kind {
            ProcessNodeKind::ServiceTask {
                input_mapping,
                output_mapping,
                verification,
                timeout_seconds,
                ..
            } => {
                ensure!(
                    (1..=600).contains(timeout_seconds),
                    "service timeout outside 1..=600 seconds"
                );
                validate_mapping(input_mapping)?;
                validate_mapping(output_mapping)?;
                if let tentaflow_protocol::processes::ActivityVerification::Condition {
                    expression,
                } = verification
                {
                    expr::validate_syntax(expression, None)?;
                }
            }
            ProcessNodeKind::UserTask { output_mapping, .. } => validate_mapping(output_mapping)?,
            _ => {}
        }
    }
    let mut flow_ids = HashSet::new();
    for flow in &model.sequence_flows {
        ensure!(
            valid_id(&flow.id) && flow_ids.insert(flow.id.as_str()),
            "invalid or duplicate sequence flow ID"
        );
        if let Some(expression) = &flow.condition {
            expr::validate_syntax(expression, None)?;
        }
    }
    let nodes = model
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    validate_diagram(model, &nodes, &flow_ids)
}

pub fn validate_model(model: &ProcessModel) -> Result<()> {
    ensure!(
        model.schema_version == 1,
        "unsupported process model schema version"
    );
    ensure!(valid_id(&model.process_id), "invalid process ID");
    ensure!(
        !model.nodes.is_empty() && model.nodes.len() <= MAX_NODES,
        "process node count exceeds B1 limit"
    );
    ensure!(
        !model.sequence_flows.is_empty() && model.sequence_flows.len() <= MAX_SEQUENCE_FLOWS,
        "process sequence flow count exceeds B1 limit"
    );
    ensure!(
        serde_json::to_vec(model)?.len() <= MAX_MODEL_BYTES,
        "process model exceeds 512 KiB"
    );
    validate_variables(&serde_json::to_value(&model.variables)?)?;

    let mut nodes = HashMap::new();
    for node in &model.nodes {
        ensure!(valid_id(&node.id), "invalid node ID: {}", node.id);
        ensure!(
            node.name.len() <= 256 && !node.name.chars().any(char::is_control),
            "invalid node name: {}",
            node.id
        );
        ensure!(
            nodes.insert(node.id.as_str(), node).is_none(),
            "duplicate node ID: {}",
            node.id
        );
        match &node.kind {
            ProcessNodeKind::ServiceTask {
                flow_id,
                input_mapping,
                output_mapping,
                verification,
                timeout_seconds,
            } => {
                ensure!(!flow_id.is_empty(), "service task {} lacks a flow", node.id);
                ensure!(
                    (1..=600).contains(timeout_seconds),
                    "service task {} timeout is outside 1..=600 seconds",
                    node.id
                );
                validate_mapping(input_mapping)?;
                validate_mapping(output_mapping)?;
                if let tentaflow_protocol::processes::ActivityVerification::Condition {
                    expression,
                } = verification
                {
                    expr::validate_syntax(expression, None)
                        .with_context(|| format!("service task {} verification", node.id))?;
                }
            }
            ProcessNodeKind::UserTask {
                assignee_user_id,
                output_mapping,
            } => {
                if let Some(user_id) = assignee_user_id {
                    ensure!(
                        !user_id.is_empty(),
                        "user task {} has empty assignee",
                        node.id
                    );
                }
                validate_mapping(output_mapping)?;
            }
            _ => {}
        }
    }
    let mut outgoing: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut incoming: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut flow_ids = HashSet::new();
    for flow in &model.sequence_flows {
        ensure!(valid_id(&flow.id), "invalid sequence flow ID: {}", flow.id);
        ensure!(
            flow_ids.insert(flow.id.as_str()),
            "duplicate sequence flow ID: {}",
            flow.id
        );
        ensure!(
            nodes.contains_key(flow.source_id.as_str())
                && nodes.contains_key(flow.target_id.as_str()),
            "sequence flow {} references missing node",
            flow.id
        );
        ensure!(
            flow.source_id != flow.target_id,
            "self-loop {} is unsupported in B1",
            flow.id
        );
        if let Some(condition) = &flow.condition {
            expr::validate_syntax(condition, None)
                .with_context(|| format!("sequence flow {} condition", flow.id))?;
        }
        outgoing
            .entry(&flow.source_id)
            .or_default()
            .push(&flow.target_id);
        incoming
            .entry(&flow.target_id)
            .or_default()
            .push(&flow.source_id);
    }

    let starts: Vec<_> = model
        .nodes
        .iter()
        .filter(|node| matches!(node.kind, ProcessNodeKind::Start))
        .collect();
    ensure!(
        starts.len() == 1,
        "process requires exactly one start event"
    );
    ensure!(
        model
            .nodes
            .iter()
            .any(|node| matches!(node.kind, ProcessNodeKind::End)),
        "process requires an end event"
    );
    for node in &model.nodes {
        let in_count = incoming.get(node.id.as_str()).map_or(0, Vec::len);
        let out_count = outgoing.get(node.id.as_str()).map_or(0, Vec::len);
        match &node.kind {
            ProcessNodeKind::Start => ensure!(
                in_count == 0 && out_count == 1,
                "start event must have one outgoing flow and no incoming flow"
            ),
            ProcessNodeKind::End => ensure!(
                out_count == 0 && in_count >= 1,
                "end event must have incoming flow and no outgoing flow"
            ),
            ProcessNodeKind::ParallelGateway => ensure!(
                (in_count == 1 && out_count >= 2) || (in_count >= 2 && out_count == 1),
                "parallel gateway {} must be a split or join",
                node.id
            ),
            ProcessNodeKind::ExclusiveGateway { default_flow_id } => {
                ensure!(
                    in_count >= 1 && out_count >= 1,
                    "exclusive gateway {} has no path",
                    node.id
                );
                if let Some(default_id) = default_flow_id {
                    ensure!(
                        model
                            .sequence_flows
                            .iter()
                            .any(|flow| flow.id == *default_id
                                && flow.source_id == node.id
                                && flow.condition.is_none()),
                        "exclusive gateway {} has invalid default flow",
                        node.id
                    );
                }
            }
            _ => ensure!(
                in_count == 1 && out_count == 1,
                "activity {} must have one incoming and outgoing flow",
                node.id
            ),
        }
        if !matches!(node.kind, ProcessNodeKind::ExclusiveGateway { .. }) {
            ensure!(
                model
                    .sequence_flows
                    .iter()
                    .filter(|flow| flow.source_id == node.id)
                    .all(|flow| flow.condition.is_none()),
                "conditions require an exclusive gateway"
            );
        }
    }
    let mut degree: HashMap<&str, usize> = nodes
        .keys()
        .map(|id| (*id, incoming.get(id).map_or(0, Vec::len)))
        .collect();
    let mut queue = VecDeque::from([starts[0].id.as_str()]);
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(node_id) = queue.pop_front() {
        order.push(node_id);
        for target in outgoing.get(node_id).into_iter().flatten() {
            let remaining = degree.get_mut(target).expect("edge target validated");
            *remaining -= 1;
            if *remaining == 0 {
                queue.push_back(target);
            }
        }
    }
    ensure!(
        order.len() == model.nodes.len(),
        "process contains a cycle or unreachable node (unsupported in B1)"
    );
    let mut can_end = HashSet::new();
    for node_id in order.iter().rev() {
        if matches!(nodes[node_id].kind, ProcessNodeKind::End)
            || outgoing
                .get(node_id)
                .into_iter()
                .flatten()
                .any(|target| can_end.contains(target))
        {
            can_end.insert(*node_id);
        }
    }
    ensure!(
        can_end.len() == nodes.len(),
        "process contains a dead-end path"
    );
    validate_diagram(model, &nodes, &flow_ids)?;
    and_pairs(model)?;
    Ok(())
}

fn validate_mapping(mapping: &std::collections::BTreeMap<String, String>) -> Result<()> {
    ensure!(
        mapping.len() <= MAX_VARIABLE_KEYS,
        "mapping exceeds 128 keys"
    );
    for (key, expression) in mapping {
        ensure!(valid_id(key), "invalid mapping key: {key}");
        expr::validate_syntax(expression, None).with_context(|| format!("mapping {key}"))?;
    }
    Ok(())
}

fn validate_diagram(
    model: &ProcessModel,
    nodes: &HashMap<&str, &tentaflow_protocol::processes::ProcessNode>,
    flow_ids: &HashSet<&str>,
) -> Result<()> {
    ensure!(
        model.diagram.shapes.len() <= model.nodes.len()
            && model.diagram.edges.len() <= model.sequence_flows.len(),
        "process diagram exceeds graph element count"
    );
    let mut shape_ids = HashSet::new();
    for shape in &model.diagram.shapes {
        ensure!(
            nodes.contains_key(shape.element_id.as_str())
                && shape_ids.insert(shape.element_id.as_str()),
            "diagram shape has missing or duplicate element"
        );
        ensure!(
            [shape.x, shape.y, shape.width, shape.height]
                .iter()
                .all(|value| value.is_finite() && value.abs() <= MAX_DI_COORDINATE)
                && shape.width > 0.0
                && shape.height > 0.0,
            "invalid process shape bounds"
        );
    }
    let mut edge_ids = HashSet::new();
    for edge in &model.diagram.edges {
        ensure!(
            flow_ids.contains(edge.sequence_flow_id.as_str())
                && edge_ids.insert(edge.sequence_flow_id.as_str()),
            "diagram edge has missing or duplicate sequence flow"
        );
        ensure!(
            edge.waypoints.len() >= 2
                && edge.waypoints.len() <= 64
                && edge.waypoints.iter().all(|point| {
                    point.x.is_finite()
                        && point.y.is_finite()
                        && point.x.abs() <= MAX_DI_COORDINATE
                        && point.y.abs() <= MAX_DI_COORDINATE
                }),
            "invalid process edge waypoints"
        );
    }
    Ok(())
}

/// Maps each parallel split to the one join reached by every branch.
pub fn and_pairs(model: &ProcessModel) -> Result<HashMap<String, String>> {
    let outgoing = |id: &str| {
        model
            .sequence_flows
            .iter()
            .filter(|flow| flow.source_id == id)
            .map(|flow| flow.target_id.as_str())
            .collect::<Vec<_>>()
    };
    let incoming = |id: &str| {
        model
            .sequence_flows
            .iter()
            .filter(|flow| flow.target_id == id)
            .count()
    };
    let joins: Vec<_> = model
        .nodes
        .iter()
        .filter(|node| {
            matches!(node.kind, ProcessNodeKind::ParallelGateway) && incoming(&node.id) >= 2
        })
        .collect();
    let splits: Vec<_> = model
        .nodes
        .iter()
        .filter(|node| {
            matches!(node.kind, ProcessNodeKind::ParallelGateway) && outgoing(&node.id).len() >= 2
        })
        .collect();
    ensure!(
        joins.len() == splits.len(),
        "parallel gateways must have paired splits and joins"
    );
    let mut pairs = HashMap::new();
    let mut used_joins = HashSet::new();
    for split in splits {
        let branches = outgoing(&split.id);
        let mut candidates = Vec::new();
        for join in &joins {
            if incoming(&join.id) != branches.len() {
                continue;
            }
            let mut branch_paths = Vec::new();
            let mut valid = true;
            for branch in &branches {
                let mut seen = HashSet::new();
                let mut stack = vec![*branch];
                while let Some(id) = stack.pop() {
                    if id == join.id {
                        continue;
                    }
                    if !seen.insert(id) {
                        continue;
                    }
                    let next = outgoing(id);
                    if next.is_empty() {
                        valid = false;
                        break;
                    }
                    stack.extend(next);
                }
                branch_paths.push(seen);
            }
            if valid
                && branch_paths.iter().enumerate().all(|(i, path)| {
                    branch_paths
                        .iter()
                        .skip(i + 1)
                        .all(|other| path.is_disjoint(other))
                })
            {
                candidates.push(join.id.clone());
            }
        }
        ensure!(
            candidates.len() == 1,
            "parallel split {} requires one structurally paired join",
            split.id
        );
        let join = candidates.pop().expect("one candidate checked");
        ensure!(
            used_joins.insert(join.clone()),
            "parallel join {join} is paired twice"
        );
        pairs.insert(split.id.clone(), join);
    }
    if used_joins.len() != joins.len() {
        bail!("unpaired parallel join");
    }
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_publishes_and_cycle_is_rejected() {
        let model = starter_model();
        validate_model(&model).unwrap();
        let mut cyclic = model;
        cyclic
            .sequence_flows
            .push(tentaflow_protocol::processes::ProcessSequenceFlow {
                id: "Flow_2".into(),
                source_id: "End_1".into(),
                target_id: "Start_1".into(),
                condition: None,
            });
        assert!(validate_model(&cyclic).is_err());
    }

    #[test]
    fn diagram_rejects_huge_finite_bounds_and_waypoints() {
        use tentaflow_protocol::processes::{ProcessEdgeDiagram, ProcessPoint, ProcessShape};

        let mut model = starter_model();
        model.diagram.shapes.push(ProcessShape {
            element_id: "Start_1".into(),
            x: 25.0,
            y: 30.0,
            width: 40.0,
            height: 35.0,
        });
        model.diagram.edges.push(ProcessEdgeDiagram {
            sequence_flow_id: "Flow_1".into(),
            waypoints: vec![
                ProcessPoint { x: 65.0, y: 47.0 },
                ProcessPoint { x: 100.0, y: 47.0 },
            ],
        });
        validate_model(&model).unwrap();
        model.diagram.shapes[0].x = 1e300;
        assert!(validate_model(&model).is_err());
        model.diagram.shapes[0].x = 25.0;
        model.diagram.edges[0].waypoints[1].y = -1e300;
        assert!(validate_model(&model).is_err());
    }
}
