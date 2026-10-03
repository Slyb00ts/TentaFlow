// ============ File: model.rs — B1 process graph validation and structured parallel joins ============

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use anyhow::{bail, ensure, Context, Result};
use tentaflow_protocol::processes::{
    ProcessDiagram, ProcessMessageTargetSpec, ProcessModel, ProcessNode, ProcessNodeKind,
    ProcessSequenceFlow, ProcessTimerSpec,
};

use crate::flow_engine::expr;
use crate::project_studio::schedules::parse_timezone;

pub const MAX_MODEL_BYTES: usize = 512 * 1024;
pub const MAX_NODES: usize = 128;
pub const MAX_SEQUENCE_FLOWS: usize = 256;
pub const MAX_VARIABLE_BYTES: usize = 256 * 1024;
pub const MAX_VARIABLE_KEYS: usize = 128;
const MAX_DI_COORDINATE: f64 = 1_000_000.0;

pub fn scope_body<'a>(
    model: &'a ProcessModel,
    subprocess_node_ids: &[String],
) -> Result<(&'a [ProcessNode], &'a [ProcessSequenceFlow], &'a BTreeMap<String, serde_json::Value>)> {
    let mut nodes = model.nodes.as_slice();
    let mut flows = model.sequence_flows.as_slice();
    let mut variables = &model.variables;
    for node_id in subprocess_node_ids {
        let node = nodes.iter().find(|node| node.id == *node_id)
            .with_context(|| format!("subprocess {node_id} is outside its parent body"))?;
        let ProcessNodeKind::SubProcess { body, .. } = &node.kind else {
            bail!("scope path node {node_id} is not a subprocess");
        };
        nodes = &body.nodes;
        flows = &body.sequence_flows;
        variables = &body.variables;
    }
    Ok((nodes, flows, variables))
}

pub fn all_nodes(model: &ProcessModel) -> Vec<&ProcessNode> {
    fn visit<'a>(nodes: &'a [ProcessNode], result: &mut Vec<&'a ProcessNode>) {
        for node in nodes {
            result.push(node);
            if let ProcessNodeKind::SubProcess { body, .. } = &node.kind {
                visit(&body.nodes, result);
            }
        }
    }
    let mut nodes = Vec::new();
    visit(&model.nodes, &mut nodes);
    nodes
}

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
        work_calendar: None,
        calendar_pin: None,
        messages: Vec::new(),
        errors: Vec::new(),
        target_namespace: None,
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

fn validate_declarations<'a>(model: &'a ProcessModel, ids: &mut HashSet<&'a str>) -> Result<()> {
    if let Some(namespace) = &model.target_namespace {
        ensure!(
            !namespace.is_empty()
                && namespace.len() <= 1024
                && !namespace.chars().any(char::is_whitespace)
                && !namespace.chars().any(char::is_control),
            "invalid process target namespace"
        );
        let parsed = url::Url::parse(namespace).context("invalid process target namespace")?;
        ensure!(!parsed.scheme().is_empty(), "process namespace must be absolute");
    }
    ensure!(
        model.messages.len() <= 32 && model.errors.len() <= 32,
        "process declaration limit exceeded"
    );
    let mut names = HashSet::new();
    for message in &model.messages {
        ensure!(
            valid_id(&message.message_id) && ids.insert(message.message_id.as_str()),
            "invalid or duplicate BPMN ID: {}",
            message.message_id
        );
        ensure!(
            !message.name.is_empty()
                && message.name.len() <= 256
                && !message.name.chars().any(char::is_control)
                && names.insert(message.name.as_str()),
            "invalid or duplicate message name: {}",
            message.name
        );
    }
    let mut codes = HashSet::new();
    for error in &model.errors {
        ensure!(
            valid_id(&error.error_id) && ids.insert(error.error_id.as_str()),
            "invalid or duplicate BPMN ID: {}",
            error.error_id
        );
        ensure!(
            !error.name.is_empty()
                && error.name.len() <= 256
                && !error.name.chars().any(char::is_control),
            "invalid error name: {}",
            error.error_id
        );
        ensure!(
            (1..=64).contains(&error.error_code.len())
                && error
                    .error_code
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b':' | b'-'))
                && codes.insert(error.error_code.as_str()),
            "invalid or duplicate error code: {}",
            error.error_code
        );
    }
    Ok(())
}

fn validate_expression(expression: &str, context: &str, required: bool) -> Result<()> {
    if expression.is_empty() && !required {
        return Ok(());
    }
    ensure!(!expression.is_empty() && expression.len() <= 4096, "invalid {context} length");
    expr::validate_syntax(expression, None).with_context(|| context.to_string())
}

fn validate_message_target(target: &ProcessMessageTargetSpec, complete: bool) -> Result<()> {
    match target {
        ProcessMessageTargetSpec::Start { definition_id } => {
            if complete || !definition_id.is_empty() {
                uuid::Uuid::parse_str(definition_id).context("invalid message start target definition")?;
            }
        }
        ProcessMessageTargetSpec::Catch {
            definition_id,
            instance_id_expression,
            subscription_id_expression,
        } => {
            if complete || !definition_id.is_empty() {
                uuid::Uuid::parse_str(definition_id).context("invalid message catch target definition")?;
            }
            ensure!(
                subscription_id_expression.is_none() || instance_id_expression.is_some(),
                "subscription target requires an instance expression"
            );
            if let Some(expression) = instance_id_expression {
                validate_expression(expression, "message target instance expression", complete)?;
            }
            if let Some(expression) = subscription_id_expression {
                validate_expression(expression, "message target subscription expression", complete)?;
            }
        }
    }
    Ok(())
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
        ProcessTimerSpec::WorkingDuration { seconds } => ensure!(
            (1..=31_536_000).contains(seconds),
            "working timer duration must be 1..=31536000 seconds"
        ),
    }
    Ok(())
}

fn validate_timer_model(model: &ProcessModel) -> Result<()> {
    let has_timer = validate_timer_nodes(&model.nodes, model.work_calendar.is_some())?;
    if let Some(calendar) = &model.work_calendar {
        super::calendar::validate_work_calendar(calendar)?;
        let bytes = serde_json::to_vec(&serde_json::json!({
            "work_calendar": calendar,
            "calendar_pin": &model.calendar_pin,
        }))?;
        ensure!(bytes.len() <= 128 * 1024, "process calendar and pin exceed 128 KiB");
    }
    super::calendar::calendar_pin_state(model)?;
    if has_timer || model.work_calendar.is_some() {
        let timezone = model
            .timer_timezone
            .as_deref()
            .context("timed process or calendar requires an explicit IANA timezone")?;
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

fn validate_timer_nodes(nodes: &[ProcessNode], has_calendar: bool) -> Result<bool> {
    let mut has_timer = false;
    for node in nodes {
        match &node.kind {
            ProcessNodeKind::TimerStart { timer } => {
                validate_timer_spec(timer, true)
                    .with_context(|| format!("timer start {}", node.id))?;
                if matches!(timer, ProcessTimerSpec::WorkingDuration { .. }) {
                    ensure!(has_calendar, "working timer requires a configured calendar");
                }
                has_timer = true;
            }
            ProcessNodeKind::TimerCatch { timer }
            | ProcessNodeKind::BoundaryTimer { timer, .. } => {
                validate_timer_spec(timer, false)
                    .with_context(|| format!("timer event {}", node.id))?;
                if matches!(timer, ProcessTimerSpec::WorkingDuration { .. }) {
                    ensure!(has_calendar, "working timer requires a configured calendar");
                }
                has_timer = true;
            }
            ProcessNodeKind::SubProcess { body, .. } => {
                has_timer |= validate_timer_nodes(&body.nodes, has_calendar)?;
            }
            _ => {}
        }
    }
    Ok(has_timer)
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
        serde_json::to_vec(model)?.len() <= MAX_MODEL_BYTES,
        "process draft exceeds 512 KiB"
    );
    validate_timer_model(model)?;
    let mut all_ids = HashSet::from([model.process_id.as_str()]);
    validate_declarations(model, &mut all_ids)?;
    let mut node_count = 0;
    let mut flow_count = 0;
    validate_draft_body(
        &model.nodes, &model.sequence_flows, &model.variables, &model.diagram,
        0, &mut all_ids, &mut node_count, &mut flow_count,
    )
}

fn validate_draft_body<'a>(
    nodes: &'a [ProcessNode],
    flows: &'a [ProcessSequenceFlow],
    variables: &BTreeMap<String, serde_json::Value>,
    diagram: &ProcessDiagram,
    depth: usize,
    all_ids: &mut HashSet<&'a str>,
    node_count: &mut usize,
    flow_count: &mut usize,
) -> Result<()> {
    ensure!(depth <= 3, "embedded subprocess depth exceeds three levels");
    *node_count += nodes.len();
    *flow_count += flows.len();
    ensure!(*node_count <= MAX_NODES && *flow_count <= MAX_SEQUENCE_FLOWS,
        "process draft exceeds whole-tree graph limits");
    validate_variables(&serde_json::to_value(variables)?)?;
    for node in nodes {
        ensure!(valid_id(&node.id) && all_ids.insert(node.id.as_str()),
            "invalid or duplicate BPMN ID: {}", node.id);
        ensure!(node.name.len() <= 256 && !node.name.chars().any(char::is_control),
            "invalid node name");
        match &node.kind {
            ProcessNodeKind::ServiceTask { input_mapping, output_mapping, verification,
                timeout_seconds, result_expression, .. } => {
                ensure!((1..=600).contains(timeout_seconds),
                    "service timeout outside 1..=600 seconds");
                validate_mapping(input_mapping)?;
                validate_mapping(output_mapping)?;
                if let tentaflow_protocol::processes::ActivityVerification::Condition { expression } = verification {
                    expr::validate_syntax(expression, None)?;
                }
                if let Some(expression) = result_expression {
                    validate_expression(expression, "service result expression", false)?;
                }
            }
            ProcessNodeKind::UserTask { output_mapping, .. }
            | ProcessNodeKind::MessageStart { output_mapping, .. }
            | ProcessNodeKind::BoundaryError { output_mapping, .. } => validate_mapping(output_mapping)?,
            ProcessNodeKind::MessageCatch { correlation_expression, output_mapping, .. }
            | ProcessNodeKind::BoundaryMessage { correlation_expression, output_mapping, .. } => {
                validate_expression(correlation_expression, "message correlation expression", false)?;
                validate_mapping(output_mapping)?;
            }
            ProcessNodeKind::MessageThrow { target, correlation_expression, payload_expression,
                ttl_seconds, .. } => {
                validate_message_target(target, false)?;
                validate_expression(correlation_expression, "message correlation expression", false)?;
                validate_expression(payload_expression, "message payload expression", false)?;
                ensure!((1..=604_800).contains(ttl_seconds),
                    "message TTL outside 1..=604800 seconds");
            }
            ProcessNodeKind::SubProcess { body, input_mapping, output_mapping } => {
                validate_mapping(input_mapping)?;
                validate_mapping(output_mapping)?;
                validate_draft_body(&body.nodes, &body.sequence_flows, &body.variables,
                    &body.diagram, depth + 1, all_ids, node_count, flow_count)?;
            }
            _ => {}
        }
        if depth > 0 {
            ensure!(!matches!(node.kind, ProcessNodeKind::TimerStart { .. }
                | ProcessNodeKind::MessageStart { .. }),
                "timer and message starts are root-only: {}", node.id);
        }
    }
    let mut flow_ids = HashSet::new();
    for flow in flows {
        ensure!(valid_id(&flow.id) && flow_ids.insert(flow.id.as_str())
            && all_ids.insert(flow.id.as_str()),
            "invalid or duplicate BPMN ID: {}", flow.id);
        if let Some(expression) = &flow.condition {
            validate_expression(expression, "sequence flow condition", true)?;
        }
    }
    let node_map = nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    validate_diagram(diagram, nodes, flows, &node_map, &flow_ids)
}

pub fn validate_model(model: &ProcessModel) -> Result<()> {
    validate_draft(model)?;
    let message_ids: HashSet<_> = model.messages.iter().map(|message| message.message_id.as_str()).collect();
    let error_ids: HashSet<_> = model.errors.iter().map(|error| error.error_id.as_str()).collect();
    let mut used_messages = HashSet::new();
    let mut used_errors = HashSet::new();
    validate_body(
        &model.nodes, &model.sequence_flows, &model.diagram, 0,
        &message_ids, &error_ids, &mut used_messages, &mut used_errors,
    )?;
    ensure!(message_ids == used_messages, "unreferenced message declaration");
    ensure!(error_ids == used_errors, "unreferenced error declaration");
    Ok(())
}

fn validate_body<'a>(
    graph_nodes: &'a [ProcessNode],
    graph_flows: &'a [ProcessSequenceFlow],
    diagram: &ProcessDiagram,
    depth: usize,
    message_ids: &HashSet<&str>,
    error_ids: &HashSet<&str>,
    used_messages: &mut HashSet<&'a str>,
    used_errors: &mut HashSet<&'a str>,
) -> Result<()> {
    ensure!(!graph_nodes.is_empty() && !graph_flows.is_empty(),
        "process body requires nodes and sequence flows");
    let mut nodes = HashMap::new();
    let mut boundary_error_handlers = HashSet::new();
    for node in graph_nodes {
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
                result_expression,
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
                if let Some(expression) = result_expression {
                    validate_expression(expression, "service result expression", true)?;
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
            ProcessNodeKind::MessageStart { message_ref, output_mapping } => {
                ensure!(message_ids.contains(message_ref.as_str()), "message start {} references an unknown declaration", node.id);
                used_messages.insert(message_ref.as_str());
                validate_mapping(output_mapping)?;
            }
            ProcessNodeKind::MessageCatch { message_ref, correlation_expression, output_mapping }
            | ProcessNodeKind::BoundaryMessage { message_ref, correlation_expression, output_mapping, .. } => {
                ensure!(message_ids.contains(message_ref.as_str()), "message event {} references an unknown declaration", node.id);
                used_messages.insert(message_ref.as_str());
                validate_expression(correlation_expression, "message correlation expression", true)?;
                validate_mapping(output_mapping)?;
            }
            ProcessNodeKind::MessageThrow { message_ref, target, correlation_expression, payload_expression, ttl_seconds } => {
                ensure!(message_ids.contains(message_ref.as_str()), "message throw {} references an unknown declaration", node.id);
                used_messages.insert(message_ref.as_str());
                validate_message_target(target, true)?;
                validate_expression(correlation_expression, "message correlation expression", true)?;
                validate_expression(payload_expression, "message payload expression", true)?;
                ensure!((1..=604_800).contains(ttl_seconds), "message TTL outside 1..=604800 seconds");
            }
            ProcessNodeKind::BoundaryError { attached_to_id, error_ref, output_mapping } => {
                if let Some(reference) = error_ref {
                    ensure!(error_ids.contains(reference.as_str()), "boundary error {} references an unknown declaration", node.id);
                    used_errors.insert(reference.as_str());
                }
                ensure!(boundary_error_handlers.insert((attached_to_id.as_str(), error_ref.as_deref())), "duplicate boundary error handler on {}", attached_to_id);
                validate_mapping(output_mapping)?;
            }
            ProcessNodeKind::SubProcess { body, input_mapping, output_mapping } => {
                ensure!(depth < 3, "embedded subprocess depth exceeds three levels");
                validate_mapping(input_mapping)?;
                validate_mapping(output_mapping)?;
                validate_body(&body.nodes, &body.sequence_flows, &body.diagram,
                    depth + 1, message_ids, error_ids, used_messages, used_errors)
                    .with_context(|| format!("embedded subprocess {}", node.id))?;
            }
            _ => {}
        }
    }
    let mut outgoing: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut incoming: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut flow_ids = HashSet::new();
    for flow in graph_flows {
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

    let starts: Vec<_> = graph_nodes.iter()
        .filter(|node| matches!(node.kind, ProcessNodeKind::Start | ProcessNodeKind::TimerStart { .. } | ProcessNodeKind::MessageStart { .. }))
        .collect();
    if depth > 0 {
        ensure!(starts.iter().all(|node| matches!(node.kind, ProcessNodeKind::Start)),
            "timer and message starts are root-only");
    }
    ensure!(
        starts.len() == 1,
        "process requires exactly one start event"
    );
    ensure!(
        graph_nodes.iter()
            .any(|node| matches!(node.kind, ProcessNodeKind::End)),
        "process requires an end event"
    );
    for node in graph_nodes {
        let in_count = incoming.get(node.id.as_str()).map_or(0, Vec::len);
        let out_count = outgoing.get(node.id.as_str()).map_or(0, Vec::len);
        match &node.kind {
            ProcessNodeKind::Start | ProcessNodeKind::TimerStart { .. } | ProcessNodeKind::MessageStart { .. } => ensure!(
                in_count == 0 && out_count == 1,
                "start event must have one outgoing flow and no incoming flow"
            ),
            ProcessNodeKind::BoundaryTimer { attached_to_id, .. }
            | ProcessNodeKind::BoundaryMessage { attached_to_id, .. }
            | ProcessNodeKind::BoundaryError { attached_to_id, .. } => {
                ensure!(
                    in_count == 0 && out_count == 1,
                    "boundary event {} needs one outgoing flow and no incoming flow",
                    node.id
                );
                ensure!(
                    matches!(nodes.get(attached_to_id.as_str()).map(|node| &node.kind),
                        Some(ProcessNodeKind::ServiceTask { .. } | ProcessNodeKind::SubProcess { .. }))
                        || (!matches!(node.kind, ProcessNodeKind::BoundaryError { .. })
                            && matches!(nodes.get(attached_to_id.as_str()).map(|node| &node.kind),
                                Some(ProcessNodeKind::UserTask { .. }))),
                    "boundary event {} has an unsupported attachment",
                    node.id
                );
            }
            ProcessNodeKind::EventBasedGateway => {
                ensure!(in_count == 1 && (2..=8).contains(&out_count), "event gateway {} needs one incoming and 2..=8 outgoing flows", node.id);
                for flow in graph_flows.iter().filter(|flow| flow.source_id == node.id) {
                    ensure!(flow.condition.is_none(), "event gateway {} cannot have conditions", node.id);
                    let branch = nodes[flow.target_id.as_str()];
                    ensure!(
                        matches!(branch.kind, ProcessNodeKind::MessageCatch { .. })
                            || matches!(&branch.kind, ProcessNodeKind::TimerCatch { timer } if matches!(timer, ProcessTimerSpec::Date { .. } | ProcessTimerSpec::Duration { .. } | ProcessTimerSpec::WorkingDuration { .. })),
                        "event gateway {} must branch directly to one-shot catches",
                        node.id
                    );
                    ensure!(incoming.get(branch.id.as_str()).map_or(0, Vec::len) == 1
                        && outgoing.get(branch.id.as_str()).map_or(0, Vec::len) == 1,
                        "event gateway child {} must have one incoming and outgoing flow",
                        branch.id
                    );
                }
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
                        graph_flows.iter()
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
                graph_flows.iter()
                    .filter(|flow| flow.source_id == node.id)
                    .all(|flow| flow.condition.is_none()),
                "conditions require an exclusive gateway"
            );
        }
    }
    let pairs = and_pairs(graph_nodes, graph_flows)?;
    let joins: HashMap<&str, &str> = pairs
        .iter()
        .map(|(split, join)| (join.as_str(), split.as_str()))
        .collect();
    let mut graph_outgoing = outgoing.clone();
    let mut degree: HashMap<&str, usize> = nodes
        .keys()
        .map(|id| (*id, incoming.get(id).map_or(0, Vec::len)))
        .collect();
    for node in graph_nodes {
        if let ProcessNodeKind::BoundaryTimer { attached_to_id, .. }
        | ProcessNodeKind::BoundaryMessage { attached_to_id, .. }
        | ProcessNodeKind::BoundaryError { attached_to_id, .. } = &node.kind {
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
        order.len() == graph_nodes.len(),
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
        for boundary in graph_nodes.iter().filter(|candidate| {
            matches!(&candidate.kind,
                ProcessNodeKind::BoundaryTimer { attached_to_id, .. }
                | ProcessNodeKind::BoundaryMessage { attached_to_id, .. }
                | ProcessNodeKind::BoundaryError { attached_to_id, .. }
                if attached_to_id.as_str() == *node_id)
        }) {
            ensure!(
                stack.is_empty(),
                "boundary event {} attaches inside an active parallel fork",
                boundary.id
            );
            states.insert(boundary.id.as_str(), (boundary.id.clone(), Vec::new()));
        }
    }
    validate_event_gateway_regions(graph_nodes, graph_flows, &nodes, &outgoing, &order)?;
    validate_diagram(diagram, graph_nodes, graph_flows, &nodes, &flow_ids)?;
    Ok(())
}

fn validate_event_gateway_regions<'a>(
    graph_nodes: &'a [ProcessNode],
    graph_flows: &[ProcessSequenceFlow],
    nodes: &HashMap<&'a str, &'a tentaflow_protocol::processes::ProcessNode>,
    outgoing: &HashMap<&'a str, Vec<&'a str>>,
    order: &[&'a str],
) -> Result<()> {
    for gateway in graph_nodes.iter().filter(|node| matches!(node.kind, ProcessNodeKind::EventBasedGateway)) {
        let branches = outgoing.get(gateway.id.as_str()).context("event gateway lacks branches")?;
        let mut branch_reach = Vec::with_capacity(branches.len());
        for branch in branches {
            let mut reached = HashSet::new();
            let mut queue = VecDeque::from([*branch]);
            while let Some(current) = queue.pop_front() {
                if reached.insert(current) {
                    queue.extend(outgoing.get(current).into_iter().flatten().copied());
                }
            }
            branch_reach.push(reached);
        }
        let common = order.iter().copied().find(|node_id| {
            branch_reach.iter().all(|reach| reach.contains(node_id))
        }).with_context(|| format!("event gateway {} has no common exclusive merge or end", gateway.id))?;
        ensure!(
            matches!(nodes[common].kind, ProcessNodeKind::End)
                || matches!(&nodes[common].kind, ProcessNodeKind::ExclusiveGateway { default_flow_id: None }
                    if outgoing.get(common).map_or(0, Vec::len) == 1
                        && graph_flows.iter().filter(|flow| flow.source_id == common).all(|flow| flow.condition.is_none())),
            "event gateway {} branches merge at unsupported node {}",
            gateway.id,
            common
        );
        let mut visited_regions = HashSet::new();
        for branch in branches {
            let mut reached = HashSet::new();
            let mut queue = VecDeque::from([*branch]);
            while let Some(current) = queue.pop_front() {
                if current == common || !reached.insert(current) {
                    continue;
                }
                ensure!(!matches!(nodes[current].kind, ProcessNodeKind::End),
                    "event gateway {} branch ends before common merge {}", gateway.id, common);
                ensure!(!matches!(nodes[current].kind, ProcessNodeKind::EventBasedGateway),
                    "event gateway {} has a nested event race", gateway.id);
                ensure!(visited_regions.insert(current),
                    "event gateway {} branches share node {} before merge", gateway.id, current);
                queue.extend(outgoing.get(current).into_iter().flatten().copied());
            }
        }
    }
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
    diagram: &ProcessDiagram,
    graph_nodes: &[ProcessNode],
    graph_flows: &[ProcessSequenceFlow],
    nodes: &HashMap<&str, &ProcessNode>,
    flow_ids: &HashSet<&str>,
) -> Result<()> {
    ensure!(
        diagram.shapes.len() <= graph_nodes.len()
            && diagram.edges.len() <= graph_flows.len(),
        "process diagram exceeds graph element count"
    );
    let mut shape_ids = HashSet::new();
    for shape in &diagram.shapes {
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
    for edge in &diagram.edges {
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
pub fn and_pairs(nodes: &[ProcessNode], flows: &[ProcessSequenceFlow]) -> Result<HashMap<String, String>> {
    let outgoing = |id: &str| {
        flows
            .iter()
            .filter(|flow| flow.source_id == id)
            .map(|flow| flow.target_id.as_str())
            .collect::<Vec<_>>()
    };
    let incoming = |id: &str| {
        flows
            .iter()
            .filter(|flow| flow.target_id == id)
            .count()
    };
    let joins: Vec<_> = nodes
        .iter()
        .filter(|node| {
            matches!(node.kind, ProcessNodeKind::ParallelGateway) && incoming(&node.id) >= 2
        })
        .collect();
    let splits: Vec<_> = nodes
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
    fn message_declarations_keep_incomplete_drafts_but_publication_requires_real_references() {
        use tentaflow_protocol::processes::{ProcessErrorDeclaration, ProcessMessageDeclaration};
        let mut model = starter_model();
        model.target_namespace = Some("urn:example:customer:v1".into());
        model.messages.push(ProcessMessageDeclaration { message_id: "Message_Order".into(), name: "order.received".into() });
        validate_draft(&model).unwrap();
        assert!(validate_model(&model).is_err(), "unused executable declaration cannot publish");
        model.nodes[0].kind = ProcessNodeKind::MessageStart { message_ref: "Message_Order".into(), output_mapping: Default::default() };
        validate_model(&model).unwrap();
        model.nodes[0].kind = ProcessNodeKind::MessageStart { message_ref: "Missing".into(), output_mapping: Default::default() };
        validate_draft(&model).unwrap();
        assert!(validate_model(&model).is_err(), "a draft may retain a deleted reference but publication cannot");
        model.nodes[0].kind = ProcessNodeKind::MessageStart { message_ref: "Message_Order".into(), output_mapping: Default::default() };
        model.errors.push(ProcessErrorDeclaration { error_id: "Message_Order".into(), name: "bad".into(), error_code: "BUSINESS".into() });
        assert!(validate_draft(&model).is_err(), "XML IDs are unique across declarations and nodes");
        model.errors[0].error_id = "Error_Business".into();
        assert!(validate_model(&model).is_err(), "an unused error declaration cannot publish");
    }

    #[test]
    fn event_gateway_accepts_disjoint_one_shot_branches_and_rejects_shared_activity() {
        use tentaflow_protocol::processes::{ProcessMessageDeclaration, ProcessNode, ProcessSequenceFlow};
        let mut model = starter_model();
        model.timer_timezone = Some("UTC".into());
        model.messages.push(ProcessMessageDeclaration { message_id: "Message_1".into(), name: "signal".into() });
        model.nodes.extend([
            ProcessNode { id: "Race_1".into(), name: "First event".into(), kind: ProcessNodeKind::EventBasedGateway },
            ProcessNode { id: "Catch_Message".into(), name: "Message".into(), kind: ProcessNodeKind::MessageCatch {
                message_ref: "Message_1".into(), correlation_expression: "vars.case_id".into(), output_mapping: Default::default(),
            } },
            ProcessNode { id: "Catch_Timer".into(), name: "Timeout".into(), kind: ProcessNodeKind::TimerCatch {
                timer: ProcessTimerSpec::Duration { seconds: 60 },
            } },
        ]);
        model.sequence_flows[0].target_id = "Race_1".into();
        for (id, source, target) in [
            ("Flow_2", "Race_1", "Catch_Message"), ("Flow_3", "Race_1", "Catch_Timer"),
            ("Flow_4", "Catch_Message", "End_1"), ("Flow_5", "Catch_Timer", "End_1"),
        ] {
            model.sequence_flows.push(ProcessSequenceFlow { id: id.into(), source_id: source.into(), target_id: target.into(), condition: None });
        }
        validate_model(&model).unwrap();
        model.sequence_flows[4].target_id = "Catch_Message".into();
        assert!(validate_model(&model).is_err(), "a race child cannot be shared by both alternatives");
    }

    #[test]
    fn calendar_without_timer_requires_zone_and_working_rule_requires_calendar() {
        use tentaflow_protocol::processes::{HolidayPolicy, ProcessWorkCalendar, WorkWindow};
        let mut model = starter_model();
        model.work_calendar = Some(ProcessWorkCalendar {
            name: "Office".into(),
            weekly_windows: vec![WorkWindow { weekday: 1, start_minute: 540, end_minute: 1020 }],
            manual_days_off: Vec::new(), holiday_policy: HolidayPolicy::None,
        });
        assert!(validate_model(&model).is_err());
        model.timer_timezone = Some("Europe/Warsaw".into());
        validate_model(&model).unwrap();
        model.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::WorkingDuration { seconds: 3600 },
        };
        validate_model(&model).unwrap();
        model.work_calendar = None;
        assert!(validate_model(&model).is_err());
        model.work_calendar = Some(ProcessWorkCalendar {
            name: "Office".into(), weekly_windows: Vec::new(),
            manual_days_off: Vec::new(), holiday_policy: HolidayPolicy::None,
        });
        assert!(validate_model(&model).is_err());
    }

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

    fn embedded_model() -> ProcessModel {
        use tentaflow_protocol::processes::ProcessSubProcess;
        let mut model = starter_model();
        model.nodes.insert(1, ProcessNode {
            id: "Sub_1".into(), name: "Review scope".into(),
            kind: ProcessNodeKind::SubProcess {
                body: ProcessSubProcess {
                    nodes: vec![
                        ProcessNode { id: "LocalStart".into(), name: "Start".into(), kind: ProcessNodeKind::Start },
                        ProcessNode { id: "LocalTask".into(), name: "Approve".into(),
                            kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } },
                        ProcessNode { id: "LocalEnd".into(), name: "End".into(), kind: ProcessNodeKind::End },
                    ],
                    sequence_flows: vec![
                        ProcessSequenceFlow { id: "LocalFlow1".into(), source_id: "LocalStart".into(), target_id: "LocalTask".into(), condition: None },
                        ProcessSequenceFlow { id: "LocalFlow2".into(), source_id: "LocalTask".into(), target_id: "LocalEnd".into(), condition: None },
                    ],
                    variables: BTreeMap::from([("local_ID".into(), serde_json::json!("value"))]),
                    diagram: ProcessDiagram::default(),
                },
                input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
            },
        });
        model.sequence_flows[0].target_id = "Sub_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_2".into(), source_id: "Sub_1".into(), target_id: "End_1".into(), condition: None,
        });
        model
    }

    #[test]
    fn embedded_scope_has_local_graph_and_global_identity() {
        let mut model = embedded_model();
        validate_model(&model).unwrap();
        assert_eq!(scope_body(&model, &["Sub_1".into()]).unwrap().0.len(), 3);
        assert_eq!(all_nodes(&model).len(), 6);
        assert!(scope_body(&model, &["LocalTask".into()]).is_err());

        if let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes[1].kind {
            body.nodes[1].id = "End_1".into();
        }
        assert!(validate_model(&model).is_err());
        let mut model = embedded_model();
        if let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes[1].kind {
            body.sequence_flows[0].target_id = "End_1".into();
        }
        assert!(validate_model(&model).is_err());
        let mut model = embedded_model();
        if let ProcessNodeKind::SubProcess { body, .. } = &mut model.nodes[1].kind {
            body.nodes[0].kind = ProcessNodeKind::TimerStart { timer: ProcessTimerSpec::Duration { seconds: 60 } };
        }
        model.timer_timezone = Some("UTC".into());
        assert!(validate_model(&model).is_err());
    }
}
