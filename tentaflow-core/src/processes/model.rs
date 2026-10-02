// ============ File: model.rs — B1 process graph validation and structured parallel joins ============

use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{bail, ensure, Context, Result};
use tentaflow_protocol::processes::{ProcessModel, ProcessNodeKind, ProcessTimerSpec};

use crate::flow_engine::expr;
use crate::project_studio::schedules::parse_timezone;

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
        timer_timezone: None,
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

pub fn validate_timer_spec(spec: &ProcessTimerSpec, is_start: bool) -> Result<()> {
    match spec {
        ProcessTimerSpec::Date { at } => {
            let parsed = chrono::DateTime::parse_from_rfc3339(at)
                .with_context(|| format!("timer date must be RFC3339 with an offset: {at}"))?;
            let fractional_digits = at
                .split_once('.')
                .map(|(_, fraction)| {
                    fraction
                        .chars()
                        .take_while(|character| character.is_ascii_digit())
                        .count()
                })
                .unwrap_or(0);
            ensure!(
                fractional_digits <= 3 && parsed.timestamp_subsec_nanos() % 1_000_000 == 0,
                "timer date precision must be milliseconds or coarser"
            );
        }
        ProcessTimerSpec::Duration { seconds } => ensure!(
            (1..=31_536_000).contains(seconds),
            "timer duration must be 1..=31536000 seconds"
        ),
        ProcessTimerSpec::Cycle {
            seconds,
            total_firings,
        } => {
            ensure!(is_start, "repeating timer is only supported as a start event");
            ensure!(
                (300..=31_536_000).contains(seconds),
                "timer cycle must be 300..=31536000 seconds"
            );
            ensure!(
                total_firings.is_none_or(|count| count > 0),
                "timer firing count must be positive"
            );
        }
        ProcessTimerSpec::Daily {
            hour,
            minute,
            total_firings,
        } => {
            ensure!(is_start, "repeating timer is only supported as a start event");
            ensure!(*hour < 24 && *minute < 60, "invalid timer daily clock time");
            ensure!(
                total_firings.is_none_or(|count| count > 0),
                "timer firing count must be positive"
            );
        }
    }
    Ok(())
}

fn validate_timer_model(model: &ProcessModel) -> Result<()> {
    let mut has_timer = false;
    for node in &model.nodes {
        match &node.kind {
            ProcessNodeKind::TimerStart { timer } => {
                validate_timer_spec(timer, true)
                    .with_context(|| format!("timer start {}", node.id))?;
                has_timer = true;
            }
            ProcessNodeKind::TimerCatch { timer }
            | ProcessNodeKind::BoundaryTimer { timer, .. } => {
                validate_timer_spec(timer, false)
                    .with_context(|| format!("timer event {}", node.id))?;
                has_timer = true;
            }
            _ => {}
        }
    }
    if has_timer {
        let timezone = model
            .timer_timezone
            .as_deref()
            .context("timed process requires an explicit IANA timezone")?;
        ensure!(
            !timezone.is_empty() && timezone.trim() == timezone,
            "timer timezone must be a nonempty IANA name"
        );
        parse_timezone(timezone)?;
    } else {
        ensure!(
            model.timer_timezone.is_none(),
            "timerless process cannot declare a timer timezone"
        );
    }
    Ok(())
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
    validate_timer_model(model)?;
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
    validate_timer_model(model)?;

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
        .filter(|node| matches!(node.kind, ProcessNodeKind::Start | ProcessNodeKind::TimerStart { .. }))
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
            ProcessNodeKind::Start | ProcessNodeKind::TimerStart { .. } => ensure!(
                in_count == 0 && out_count == 1,
                "start event must have one outgoing flow and no incoming flow"
            ),
            ProcessNodeKind::BoundaryTimer { attached_to_id, .. } => {
                ensure!(
                    in_count == 0 && out_count == 1,
                    "boundary timer {} needs one outgoing flow and no incoming flow",
                    node.id
                );
                ensure!(
                    matches!(nodes.get(attached_to_id.as_str()).map(|node| &node.kind),
                        Some(ProcessNodeKind::UserTask { .. } | ProcessNodeKind::ServiceTask { .. })),
                    "boundary timer {} must attach to a user or service task",
                    node.id
                );
            }
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
    let pairs = and_pairs(model)?;
    let joins: HashMap<&str, &str> = pairs
        .iter()
        .map(|(split, join)| (join.as_str(), split.as_str()))
        .collect();
    let mut graph_outgoing = outgoing.clone();
    let mut degree: HashMap<&str, usize> = nodes
        .keys()
        .map(|id| (*id, incoming.get(id).map_or(0, Vec::len)))
        .collect();
    for node in &model.nodes {
        if let ProcessNodeKind::BoundaryTimer { attached_to_id, .. } = &node.kind {
            graph_outgoing
                .entry(attached_to_id.as_str())
                .or_default()
                .push(node.id.as_str());
            *degree.get_mut(node.id.as_str()).expect("boundary node validated") += 1;
        }
    }
    let mut queue = VecDeque::from([starts[0].id.as_str()]);
    let mut order = Vec::with_capacity(nodes.len());
    while let Some(node_id) = queue.pop_front() {
        order.push(node_id);
        for target in graph_outgoing.get(node_id).into_iter().flatten() {
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
            || graph_outgoing
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
    let mut states: HashMap<&str, (String, Vec<String>)> = HashMap::new();
    states.insert(starts[0].id.as_str(), ("main".into(), Vec::new()));
    for node_id in &order {
        let (region, mut stack) = states
            .get(node_id)
            .cloned()
            .context("process node has no activation region")?;
        let node = nodes[node_id];
        if matches!(node.kind, ProcessNodeKind::ParallelGateway) {
            if outgoing.get(node_id).is_some_and(|flows| flows.len() >= 2) {
                stack.push((*node_id).to_string());
            } else {
                let split = joins.get(node_id).context("parallel join lacks its paired split")?;
                ensure!(
                    stack.pop().as_deref() == Some(*split),
                    "parallel join {} has an invalid activation stack",
                    node_id
                );
            }
        }
        if matches!(node.kind, ProcessNodeKind::End) {
            ensure!(stack.is_empty(), "end event has an open parallel activation");
        }
        for target in outgoing.get(node_id).into_iter().flatten() {
            if matches!(nodes[target].kind, ProcessNodeKind::End) {
                ensure!(stack.is_empty(), "boundary or main path ends inside a parallel fork");
                states.entry(*target).or_insert_with(|| (region.clone(), stack.clone()));
            } else if let Some(existing) = states.get(target) {
                ensure!(
                    existing.0 == region && existing.1 == stack,
                    "boundary path merges with another active region at {}",
                    target
                );
            } else {
                states.insert(*target, (region.clone(), stack.clone()));
            }
        }
        for boundary in model.nodes.iter().filter(|candidate| {
            matches!(&candidate.kind, ProcessNodeKind::BoundaryTimer { attached_to_id, .. }
                if attached_to_id.as_str() == *node_id)
        }) {
            ensure!(
                stack.is_empty(),
                "boundary timer {} attaches inside an active parallel fork",
                boundary.id
            );
            states.insert(boundary.id.as_str(), (boundary.id.clone(), Vec::new()));
        }
    }
    validate_diagram(model, &nodes, &flow_ids)?;
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

    fn boundary_model() -> ProcessModel {
        use tentaflow_protocol::processes::{ProcessNode, ProcessSequenceFlow};

        let mut model = starter_model();
        model.timer_timezone = Some("UTC".into());
        model.nodes.push(ProcessNode {
            id: "Review_1".into(),
            name: "Review".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: Default::default(),
            },
        });
        model.nodes.push(ProcessNode {
            id: "Boundary_A".into(),
            name: "Time limit".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Review_1".into(),
                cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 90 },
            },
        });
        model.nodes.push(ProcessNode {
            id: "Boundary_B".into(),
            name: "Reminder".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Review_1".into(),
                cancel_activity: false,
                timer: ProcessTimerSpec::Date {
                    at: "2027-01-02T03:04:05Z".into(),
                },
            },
        });
        model.sequence_flows[0].target_id = "Review_1".into();
        for (id, source_id) in [
            ("Flow_2", "Review_1"),
            ("Flow_3", "Boundary_A"),
            ("Flow_4", "Boundary_B"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                id: id.into(),
                source_id: source_id.into(),
                target_id: "End_1".into(),
                condition: None,
            });
        }
        model
    }

    #[test]
    fn boundary_siblings_are_real_implicit_branches_without_merging_activities() {
        use tentaflow_protocol::processes::{ProcessNode, ProcessSequenceFlow};

        let mut model = boundary_model();
        validate_model(&model).unwrap();
        let boundary = model.nodes.iter_mut().find(|node| node.id == "Boundary_A").unwrap();
        if let ProcessNodeKind::BoundaryTimer { attached_to_id, .. } = &mut boundary.kind {
            *attached_to_id = "End_1".into();
        }
        assert!(validate_model(&model).is_err());
        model = boundary_model();
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_5".into(), source_id: "Review_1".into(),
            target_id: "Boundary_A".into(), condition: None,
        });
        assert!(validate_model(&model).is_err());
        model = boundary_model();
        model.nodes.push(ProcessNode {
            id: "Shared_1".into(), name: "Shared".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: None, output_mapping: Default::default(),
            },
        });
        model.sequence_flows.iter_mut().find(|flow| flow.id == "Flow_2").unwrap().target_id = "Shared_1".into();
        model.sequence_flows.iter_mut().find(|flow| flow.id == "Flow_3").unwrap().target_id = "Shared_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_5".into(), source_id: "Shared_1".into(),
            target_id: "End_1".into(), condition: None,
        });
        assert!(validate_model(&model).is_err());
    }

    #[test]
    fn boundary_after_paired_join_and_own_closed_parallel_region_are_supported() {
        use tentaflow_protocol::processes::{ProcessNode, ProcessSequenceFlow};

        let mut model = boundary_model();
        for id in ["Split_1", "Join_1", "SideSplit_1", "SideJoin_1"] {
            model.nodes.push(ProcessNode {
                id: id.into(), name: id.into(), kind: ProcessNodeKind::ParallelGateway,
            });
        }
        for id in ["Branch_A", "Branch_B", "Side_A", "Side_B"] {
            model.nodes.push(ProcessNode {
                id: id.into(), name: id.into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None, output_mapping: Default::default(),
                },
            });
        }
        model.sequence_flows.iter_mut().find(|flow| flow.id == "Flow_1").unwrap().target_id = "Split_1".into();
        model.sequence_flows.iter_mut().find(|flow| flow.id == "Flow_3").unwrap().target_id = "SideSplit_1".into();
        for (id, source, target) in [
            ("F_A", "Split_1", "Branch_A"), ("F_B", "Split_1", "Branch_B"),
            ("F_C", "Branch_A", "Join_1"), ("F_D", "Branch_B", "Join_1"),
            ("F_E", "Join_1", "Review_1"),
            ("F_F", "SideSplit_1", "Side_A"), ("F_G", "SideSplit_1", "Side_B"),
            ("F_H", "Side_A", "SideJoin_1"), ("F_I", "Side_B", "SideJoin_1"),
            ("F_J", "SideJoin_1", "End_1"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow {
                id: id.into(), source_id: source.into(), target_id: target.into(), condition: None,
            });
        }
        validate_model(&model).unwrap();
        let boundary = model.nodes.iter_mut().find(|node| node.id == "Boundary_A").unwrap();
        if let ProcessNodeKind::BoundaryTimer { attached_to_id, .. } = &mut boundary.kind {
            *attached_to_id = "Branch_A".into();
        }
        assert!(validate_model(&model).is_err());
    }

    #[test]
    fn timer_profiles_require_explicit_zone_and_supported_start_catch_rules() {
        let mut model = starter_model();
        model.nodes[0].kind = ProcessNodeKind::TimerStart { timer: ProcessTimerSpec::Cycle { seconds: 300, total_firings: Some(3) } };
        assert!(validate_model(&model).is_err());
        model.timer_timezone = Some("Europe/Warsaw".into());
        validate_model(&model).unwrap();
        model.timer_timezone = Some("Mars/Olympus".into());
        assert!(validate_model(&model).is_err());
        model.timer_timezone = Some("UTC".into());
        model.nodes[0].kind = ProcessNodeKind::TimerStart { timer: ProcessTimerSpec::Cycle { seconds: 299, total_firings: Some(3) } };
        assert!(validate_model(&model).is_err());
        model.nodes[0].kind = ProcessNodeKind::TimerStart { timer: ProcessTimerSpec::Daily { hour: 24, minute: 0, total_firings: None } };
        assert!(validate_model(&model).is_err());
        model.nodes[0].kind = ProcessNodeKind::TimerStart { timer: ProcessTimerSpec::Date { at: "2027-01-02T03:04:05.1234Z".into() } };
        assert!(validate_model(&model).is_err());
        model.nodes[0].kind = ProcessNodeKind::TimerStart { timer: ProcessTimerSpec::Date { at: "2027-01-02T03:04:05.123Z".into() } };
        validate_model(&model).unwrap();
        model.nodes.insert(1, tentaflow_protocol::processes::ProcessNode { id: "Wait_1".into(), name: "Wait".into(), kind: ProcessNodeKind::TimerCatch { timer: ProcessTimerSpec::Cycle { seconds: 300, total_firings: None } } });
        model.sequence_flows[0].target_id = "Wait_1".into();
        model.sequence_flows.push(tentaflow_protocol::processes::ProcessSequenceFlow { id: "Flow_2".into(), source_id: "Wait_1".into(), target_id: "End_1".into(), condition: None });
        assert!(validate_model(&model).is_err());
        model.nodes[1].kind = ProcessNodeKind::TimerCatch { timer: ProcessTimerSpec::Duration { seconds: 90 } };
        validate_model(&model).unwrap();
    }

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
